//! Authenticated activation of a separately verified replacement mission.
use axum::{
    Extension, Json,
    extract::{Path, State},
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    MissionPlan, PlanPayload, bad, fnv, plan_text, same, verification::revalidate_with_client,
};
use crate::{auth::CurrentAdmin, error::AppError, geo, models::Telemetry, state::AppState};

mod guards;
use guards::{MAX_POSE_AGE_SECONDS, fresh, remaining_stops_match, valid_measurement, valid_pose};

#[derive(Debug, Deserialize)]
pub(crate) struct ActivatePlan {
    pub plan_id: Uuid,
    pub expected_route_id: Uuid,
    pub expected_telemetry_id: i64,
    /// The caller must use the same altitude datum for telemetry and the plan.
    pub altitude_reference: String,
}
#[derive(Debug, Serialize)]
pub(crate) struct ActivatedPlan {
    pub delivery_id: Uuid,
    pub route_id: Uuid,
    pub plan_id: Uuid,
    pub telemetry_id: i64,
    pub route_fingerprint: String,
}
#[derive(Debug)]
struct DeliveryRow {
    drone_id: String,
    route_id: Uuid,
    status: String,
    schedule_source: String,
}
crate::db::row!(DeliveryRow {
    drone_id,
    route_id,
    status,
    schedule_source
});
#[derive(Debug)]
struct FleetRow {
    capacity_grams: i32,
    active: bool,
    cruise_speed_mps: f64,
    max_altitude_m: f64,
}
crate::db::row!(FleetRow {
    capacity_grams,
    active,
    cruise_speed_mps,
    max_altitude_m
});
#[derive(Debug, Serialize)]
pub(crate) struct StopRow {
    order_id: Uuid,
    stop_sequence: i32,
    status: String,
    #[serde(skip)]
    latitude: Option<f64>,
    #[serde(skip)]
    longitude: Option<f64>,
    #[serde(skip)]
    order_status: String,
}
crate::db::row!(StopRow {
    order_id,
    stop_sequence,
    status,
    latitude,
    longitude,
    order_status
});

pub(crate) async fn list_stops(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<StopRow>>, AppError> {
    let connection = state.pool.get().await?;
    let exists = crate::db::scalar_one::<bool>(
        &connection,
        "SELECT EXISTS(SELECT 1 FROM neurovector.deliveries WHERE id=$1)",
        &[&id],
    )
    .await?;
    if !exists {
        return Err(AppError::NotFound);
    }
    let rows = crate::db::fetch_all::<StopRow>(&connection, "SELECT s.order_id,s.stop_sequence,s.status,o.latitude,o.longitude,o.status AS order_status FROM neurovector.delivery_stops s JOIN neurovector.orders o ON o.id=s.order_id WHERE s.delivery_id=$1 ORDER BY s.stop_sequence", &[&id]).await?;
    Ok(Json(rows))
}

pub(crate) async fn activate_plan(
    State(state): State<AppState>,
    Extension(admin): Extension<CurrentAdmin>,
    Path(id): Path<Uuid>,
    Json(body): Json<ActivatePlan>,
) -> Result<Json<ActivatedPlan>, AppError> {
    if body.altitude_reference != "plan" {
        return Err(bad("altitude_reference must acknowledge the plan datum"));
    }
    let mut connection = state.pool.get().await?;
    let tx = connection.transaction().await?;
    let drone_id = crate::db::scalar_optional::<String>(
        &tx,
        "SELECT drone_id FROM neurovector.deliveries WHERE id=$1",
        &[&id],
    )
    .await?
    .ok_or(AppError::NotFound)?;
    // Match ingestion's advisory-before-row-lock order.
    crate::db::execute(
        &tx,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        &[&drone_id],
    )
    .await?;
    let delivery = crate::db::fetch_optional::<DeliveryRow>(&tx, "SELECT drone_id,route_id,status,schedule_source FROM neurovector.deliveries WHERE id=$1 FOR UPDATE", &[&id]).await?.ok_or(AppError::NotFound)?;
    if delivery.drone_id != drone_id
        || delivery.route_id != body.expected_route_id
        || !matches!(delivery.status.as_str(), "dispatched" | "in_flight")
    {
        return Err(AppError::Conflict(
            "delivery route or status changed".into(),
        ));
    }
    // Hold a table lock so inserts, removals and edits of no-fly zones cannot race validation.
    crate::db::execute(
        &tx,
        "LOCK TABLE neurovector.no_fly_zones IN SHARE MODE",
        &[],
    )
    .await?;
    let old = crate::db::fetch_optional::<MissionPlan>(
        &tx,
        "SELECT * FROM neurovector.mission_plans WHERE route_id=$1 FOR SHARE",
        &[&delivery.route_id],
    )
    .await?
    .ok_or_else(|| bad("active mission is missing"))?;
    let plan = crate::db::fetch_optional::<MissionPlan>(
        &tx,
        "SELECT * FROM neurovector.mission_plans WHERE id=$1 FOR UPDATE",
        &[&body.plan_id],
    )
    .await?
    .ok_or(AppError::NotFound)?;
    if old.drone_id != delivery.drone_id
        || plan.route_id == delivery.route_id
        || plan.status != "verified"
        || plan.verified_at.is_none()
        || old.status != "verified"
    {
        return Err(AppError::Conflict(
            "replacement and active missions must be verified and distinct".into(),
        ));
    }
    // Prevent route deletion while the checked mission is being activated.
    let _route_locks = crate::db::scalar_all::<Uuid>(
        &tx,
        "SELECT id FROM neurovector.routes WHERE id=ANY($1::uuid[]) FOR SHARE",
        &[&vec![delivery.route_id, plan.route_id]],
    )
    .await?;
    let old_payload: PlanPayload =
        serde_json::from_value(old.payload.clone()).map_err(|e| bad(e.to_string()))?;
    if fnv(&old.plan_text) != old.route_fingerprint
        || plan_text(old.id, &old.drone_id, old.expires_at, &old_payload)? != old.plan_text
    {
        return Err(bad("active mission integrity check failed"));
    }
    let (payload, validation) =
        revalidate_with_client(&state, &tx, &plan, &delivery.drone_id).await?;
    let telemetry = crate::db::fetch_optional::<Telemetry>(&tx, "SELECT id,drone_id,flight_id,ts,latitude,longitude,altitude_m,speed_mps,vertical_speed_mps,battery_pct,heading_deg,status,received_at FROM neurovector.current_drone_telemetry WHERE drone_id=$1", &[&delivery.drone_id]).await?.ok_or_else(|| AppError::Conflict("fresh held telemetry required".into()))?;
    let active_flight = crate::db::scalar_optional::<Uuid>(&tx, "SELECT id FROM neurovector.flight_sessions WHERE drone_id=$1 AND delivery_id=$2 AND ended_at IS NULL FOR UPDATE", &[&delivery.drone_id, &id]).await?;
    if telemetry.id != body.expected_telemetry_id
        || telemetry.flight_id != active_flight
        || active_flight.is_none()
    {
        return Err(AppError::Conflict(
            "current telemetry or active flight changed".into(),
        ));
    }
    let fleet = crate::db::fetch_optional::<FleetRow>(
        &tx,
        "SELECT capacity_grams,active,cruise_speed_mps,max_altitude_m FROM neurovector.delivery_fleet WHERE drone_id=$1 FOR SHARE",
        &[&delivery.drone_id],
    )
    .await?
    .ok_or_else(|| AppError::Conflict("registered fleet drone required".into()))?;
    let measurement = crate::db::fetch_optional::<(i32, DateTime<Utc>)>(&tx, "SELECT weight_grams,measured_at FROM neurovector.delivery_weight_measurements WHERE delivery_id=$1 ORDER BY measured_at DESC,id DESC LIMIT 1", &[&id]).await?;
    let stops = crate::db::fetch_all::<StopRow>(&tx, "SELECT s.order_id,s.stop_sequence,s.status,o.latitude,o.longitude,o.status AS order_status FROM neurovector.delivery_stops s JOIN neurovector.orders o ON o.id=s.order_id WHERE s.delivery_id=$1 ORDER BY s.stop_sequence FOR UPDATE OF s,o", &[&id]).await?;
    let now = Utc::now();
    if !fleet.active
        || payload.constraints.max_altitude_m > fleet.max_altitude_m
        || payload.constraints.speed_mps > fleet.cruise_speed_mps
        || !valid_measurement(measurement, fleet.capacity_grams, now)
    {
        return Err(AppError::Conflict(
            "fresh positive payload measurement within active fleet capacity required".into(),
        ));
    }
    if !valid_pose(&telemetry, &payload, validation.estimated_energy_wh, now) {
        return Err(AppError::Conflict(
            "fresh held pose, battery reserve or mission start is invalid".into(),
        ));
    }
    if !remaining_stops_match(
        &old_payload,
        &payload,
        &stops,
        delivery.schedule_source == "automatic",
    ) {
        return Err(AppError::Conflict(
            "replacement must preserve remaining customer stops and return".into(),
        ));
    }
    if plan.expires_at <= Utc::now()
        || !fresh(telemetry.ts, Utc::now(), MAX_POSE_AGE_SECONDS)
        || !fresh(telemetry.received_at, Utc::now(), MAX_POSE_AGE_SECONDS)
    {
        return Err(AppError::Conflict(
            "mission or current telemetry expired during activation".into(),
        ));
    }
    let changed = crate::db::execute(&tx, "UPDATE neurovector.deliveries SET route_id=$1,updated_at=now() WHERE id=$2 AND route_id=$3 AND status=$4", &[&plan.route_id, &id, &delivery.route_id, &delivery.status]).await?;
    if changed != 1 {
        return Err(AppError::Conflict(
            "delivery changed during activation".into(),
        ));
    }
    crate::db::execute(&tx, "INSERT INTO neurovector.order_tracking_events(order_id,delivery_stop_id,event_type,dedupe_key,payload) SELECT s.order_id,s.id,'mission_replanned','mission_replanned:' || $2::uuid::text,jsonb_build_object('previous_route_id',$3::uuid,'route_id',$4::uuid,'plan_id',$2::uuid,'telemetry_id',$5::bigint,'activated_by',$6::text) FROM neurovector.delivery_stops s WHERE s.delivery_id=$1 ON CONFLICT(order_id,dedupe_key) DO NOTHING", &[&id,&plan.id,&delivery.route_id,&plan.route_id,&telemetry.id,&admin.username]).await?;
    tx.commit().await?;
    Ok(Json(ActivatedPlan {
        delivery_id: id,
        route_id: plan.route_id,
        plan_id: plan.id,
        telemetry_id: telemetry.id,
        route_fingerprint: plan.route_fingerprint,
    }))
}

#[cfg(test)]
mod tests;
