//! Bounded candidate planning and independent drone cross-check before dispatch.
use crate::{
    error::AppError,
    geo::Waypoint,
    routes::route_plans::{
        NoFlyZoneRow, RouteInput, RouteValidation, load_active_zones, validate_lat_lon,
        validate_route_with_zones,
    },
    state::AppState,
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct WindowTarget {
    pub window: Waypoint,
    pub outward_bearing_deg: f64,
    pub standoff_m: f64,
    pub approach_distance_m: f64,
    pub hold_seconds: f64,
}
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct PlanInput {
    #[serde(flatten)]
    pub route: RouteInput,
    #[serde(default)]
    pub alternatives: Vec<Vec<Waypoint>>,
    pub window_target: Option<WindowTarget>,
    pub altitude_reference: Option<crate::models::AltitudeReference>,
    #[serde(default = "default_ttl")]
    pub expires_in_seconds: i64,
}
fn default_ttl() -> i64 {
    600
}
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Constraints {
    pub max_altitude_m: f64,
    pub max_distance_m: f64,
    pub max_leg_m: f64,
    pub speed_mps: f64,
    pub power_w: f64,
    pub battery_wh: f64,
    pub hold_seconds: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PlanPayload {
    #[serde(default)]
    pub altitude_reference: Option<crate::models::AltitudeReference>,
    pub selected_index: usize,
    pub required_stops: Vec<Waypoint>,
    pub candidates: Vec<RouteInput>,
    pub validations: Vec<RouteValidation>,
    pub constraints: Constraints,
    pub zones: Vec<NoFlyZoneRow>,
    pub window_target: Option<WindowTarget>,
    pub optimality_scope: String,
}
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct MissionPlan {
    pub id: Uuid,
    pub route_id: Uuid,
    pub drone_id: String,
    pub status: String,
    pub route_fingerprint: String,
    pub plan_text: String,
    pub payload: Value,
    pub cross_check: Option<Value>,
    pub expires_at: DateTime<Utc>,
    pub verified_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}
crate::db::row!(MissionPlan {
    id,
    route_id,
    drone_id,
    status,
    route_fingerprint,
    plan_text,
    payload,
    cross_check,
    expires_at,
    verified_at,
    created_at
});

#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CrossCheck {
    pub drone_id: String,
    pub route_fingerprint: String,
    pub valid: bool,
    pub selected_index: usize,
    pub distance_m: f64,
    pub estimated_duration_s: f64,
    pub estimated_energy_wh: f64,
}
fn bad(s: impl Into<String>) -> AppError {
    AppError::BadRequest(s.into())
}

pub(crate) mod activation;
mod detour;
mod planning;
mod verification;
mod window;
mod wire;

use detour::PreparedZones;
#[cfg(test)]
use detour::generate_detour;
pub(crate) use planning::automatic_payload;
use planning::{build_payload, constraints, contains_required};
#[cfg(test)]
use verification::delivery_stops_match;
pub use verification::verify_route_for_dispatch;
use verification::{close, revalidate};
use window::{append_window, offset, same, validate_window};
use wire::{fnv, plan_text};
pub(crate) async fn persist_pending_plan(
    tx: &deadpool_postgres::Transaction<'_>,
    route_id: Uuid,
    drone: &str,
    payload: &PlanPayload,
) -> Result<(), AppError> {
    let id = Uuid::new_v4();
    let expires = Utc::now() + Duration::seconds(1800);
    let text = plan_text(id, drone, expires, payload)?;
    let fingerprint = fnv(&text);
    let value = serde_json::to_value(payload).map_err(|e| bad(e.to_string()))?;
    crate::db::execute(tx, "INSERT INTO neurovector.mission_plans(id,route_id,drone_id,route_fingerprint,plan_text,payload,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7)", &[&id, &route_id, &drone, &fingerprint, &text, &value, &expires]).await?;
    Ok(())
}

pub async fn get_plan_by_route(
    State(state): State<AppState>,
    Path(route_id): Path<Uuid>,
) -> Result<Json<MissionPlan>, AppError> {
    let plan = crate::db::fetch_optional::<MissionPlan>(
        &state.pool.get().await?,
        "SELECT * FROM neurovector.mission_plans WHERE route_id=$1",
        &[&route_id],
    )
    .await?
    .ok_or(AppError::NotFound)?;
    Ok(Json(plan))
}

#[utoipa::path(post,path="/api/v1/routes/plan",tag="routes",request_body=PlanInput,responses((status=201,description="Bounded candidate plan pending drone cross-check",body=MissionPlan)))]
pub async fn create_plan(
    State(state): State<AppState>,
    Json(body): Json<PlanInput>,
) -> Result<(StatusCode, Json<MissionPlan>), AppError> {
    let drone = body
        .route
        .drone_id
        .as_deref()
        .ok_or_else(|| bad("drone_id is required for a mission"))?;
    if drone.is_empty()
        || drone.len() > 128
        || !drone
            .bytes()
            .all(|v| v.is_ascii_alphanumeric() || b"-_.".contains(&v))
    {
        return Err(bad("drone_id must be a 1..128 character identifier"));
    }
    if !(30..=1800).contains(&body.expires_in_seconds)
        || !(2..=254).contains(&body.route.waypoints.len())
        || body.alternatives.len() > 13
        || body
            .alternatives
            .iter()
            .any(|v| !(2..=254).contains(&v.len()))
    {
        return Err(bad("plan expiry/candidate/waypoint limits exceeded"));
    }
    if let Some(w) = &body.window_target {
        validate_window(w)?;
    }
    let end = body.route.waypoints.last().unwrap();
    if let Some(w) = &body.window_target
        && !same(end, &w.window)
    {
        return Err(bad(
            "window route final input waypoint must match window_target.window",
        ));
    }
    for w in body
        .route
        .waypoints
        .iter()
        .chain(body.alternatives.iter().flatten())
    {
        validate_lat_lon(w.lat, w.lon).map_err(bad)?;
        if !w.altitude_m.is_finite() || w.altitude_m <= 0.0 {
            return Err(bad("waypoint altitude must be finite and positive"));
        }
    }
    let zones = load_active_zones(&state.pool).await?;
    let drone = drone.to_owned();
    let expires_in_seconds = body.expires_in_seconds;
    let payload = build_payload(&state, body, zones)?;
    let id = Uuid::new_v4();
    let expires = Utc::now() + Duration::seconds(expires_in_seconds);
    let text = plan_text(id, &drone, expires, &payload)?;
    let fingerprint = fnv(&text);
    let route = &payload.candidates[payload.selected_index];
    let v = &payload.validations[payload.selected_index];
    let mut connection = state.pool.get().await?;
    let tx = connection.transaction().await?;
    let route_id:Uuid=crate::db::scalar_one(&tx, "INSERT INTO neurovector.routes(name,drone_id,waypoints,cruise_speed_mps,max_altitude_m,distance_m,estimated_duration_s) VALUES($1,$2,$3,$4,$5,$6,$7) RETURNING id", &[&(&route.name), &(drone), &(serde_json::to_value(&route.waypoints).map_err(|e|bad(e.to_string()))?), &(route.cruise_speed_mps.unwrap()), &(route.max_altitude_m.unwrap()), &(v.distance_m), &(v.estimated_duration_s)]).await?;
    let plan=crate::db::fetch_one::<MissionPlan>(&tx, "INSERT INTO neurovector.mission_plans(id,route_id,drone_id,route_fingerprint,plan_text,payload,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7) RETURNING *", &[&(id), &(route_id), &(drone), &(fingerprint), &(text), &(serde_json::to_value(payload).map_err(|e|bad(e.to_string()))?), &(expires)]).await?;
    tx.commit().await?;
    drop(connection);
    Ok((StatusCode::CREATED, Json(plan)))
}
#[utoipa::path(get,path="/api/v1/routes/plans/{id}",tag="routes",params(("id"=Uuid,Path,description="Mission plan ID")),responses((status=200,description="Persisted mission and drone protocol",body=MissionPlan)))]
pub async fn get_plan(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<MissionPlan>, AppError> {
    Ok(Json(fetch_plan(&state, id).await?))
}
async fn fetch_plan(state: &AppState, id: Uuid) -> Result<MissionPlan, AppError> {
    crate::db::fetch_optional::<MissionPlan>(
        &state.pool.get().await?,
        "SELECT * FROM neurovector.mission_plans WHERE id=$1",
        &[&(id)],
    )
    .await?
    .ok_or(AppError::NotFound)
}

#[utoipa::path(post,path="/api/v1/routes/plans/{id}/cross-check",tag="routes",params(("id"=Uuid,Path,description="Mission plan ID")),request_body=CrossCheck,responses((status=200,description="Drone and backend cross-check agree",body=MissionPlan)))]
pub async fn cross_check(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(report): Json<CrossCheck>,
) -> Result<Json<MissionPlan>, AppError> {
    let plan = fetch_plan(&state, id).await?;
    if report.drone_id != plan.drone_id {
        return Err(AppError::Conflict(
            "mission assigned to another drone".into(),
        ));
    }
    let checked = revalidate(&state, &plan, &report.drone_id).await;
    let valid = match &checked {
        Ok((p, v)) => {
            report.valid
                && report.route_fingerprint == plan.route_fingerprint
                && report.selected_index == p.selected_index
                && close(report.distance_m, v.distance_m)
                && close(report.estimated_duration_s, v.estimated_duration_s)
                && close(report.estimated_energy_wh, v.estimated_energy_wh)
        }
        Err(_) => false,
    };
    let value = serde_json::to_value(&report).map_err(|e| bad(e.to_string()))?;
    let updated=crate::db::execute(&state.pool.get().await?, "UPDATE neurovector.mission_plans SET status=$2,cross_check=$3,verified_at=CASE WHEN $2='verified' THEN now() ELSE NULL END WHERE id=$1 AND expires_at>now()", &[&(id), &(if valid{"verified"}else{"rejected"}), &(value)]).await?;
    if updated != 1 {
        return Err(AppError::Conflict(
            "mission expired during cross-check".into(),
        ));
    }
    checked?;
    if !valid {
        return Err(bad(
            "drone cross-check disagrees with route identity, optimality, validity or metrics",
        ));
    }
    Ok(Json(fetch_plan(&state, id).await?))
}

#[cfg(test)]
#[path = "mission_school_tests.rs"]
mod school_tests;
