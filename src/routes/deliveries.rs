use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, patch},
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{error::AppError, routes::route_plans::fetch_route, state::AppState};

const MAX_MEASUREMENT_AGE_SECONDS: i64 = 300;
const MAX_FUTURE_CLOCK_SKEW_SECONDS: i64 = 5;
const DELIVERY_ORDER_READINESS_GUARD_MESSAGE: &str =
    "all scheduled delivery orders must be ready before dispatch";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_deliveries).post(create_delivery))
        .route("/{id}", get(get_delivery))
        .route("/{id}/status", patch(update_status))
}

#[derive(Debug, Serialize)]
pub struct Delivery {
    pub id: Uuid,
    pub route_id: Uuid,
    pub drone_id: String,
    pub cargo: String,
    pub status: String,
    pub scheduled_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
crate::db::row!(Delivery {
    id,
    route_id,
    drone_id,
    cargo,
    status,
    scheduled_at,
    started_at,
    completed_at,
    notes,
    created_at,
    updated_at
});

#[derive(Debug, Serialize)]
pub struct DeliveryDetail {
    #[serde(flatten)]
    pub delivery: Delivery,
    pub route_name: String,
    pub route_distance_m: f64,
    pub estimated_duration_s: f64,
}

#[derive(Debug, Deserialize)]
pub struct CreateDelivery {
    pub route_id: Uuid,
    pub drone_id: String,
    pub cargo: String,
    pub scheduled_at: DateTime<Utc>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ListDeliveriesParams {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub drone_id: Option<String>,
    #[serde(default)]
    pub from: Option<DateTime<Utc>>,
    #[serde(default)]
    pub to: Option<DateTime<Utc>>,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateStatus {
    pub status: String,
    #[serde(default)]
    pub notes: Option<String>,
}

async fn list_deliveries(
    State(state): State<AppState>,
    Query(params): Query<ListDeliveriesParams>,
) -> Result<Json<Vec<Delivery>>, AppError> {
    let limit = params.limit.unwrap_or(100).clamp(1, 500);
    let deliveries = crate::db::fetch_all::<Delivery>(
        &state.pool.get().await?,
        r#"
        SELECT id, route_id, drone_id, cargo, status, scheduled_at, started_at,
               completed_at, notes, created_at, updated_at
        FROM neurovector.deliveries
        WHERE ($1::text IS NULL OR status = $1)
          AND ($2::text IS NULL OR drone_id = $2)
          AND ($3::timestamptz IS NULL OR scheduled_at >= $3)
          AND ($4::timestamptz IS NULL OR scheduled_at <= $4)
        ORDER BY scheduled_at ASC
        LIMIT $5
        "#,
        &[
            &(params.status.as_deref()),
            &(params.drone_id.as_deref()),
            &(params.from),
            &(params.to),
            &(limit),
        ],
    )
    .await?;

    Ok(Json(deliveries))
}

async fn get_delivery(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<DeliveryDetail>, AppError> {
    let row = crate::db::fetch_optional::<DeliveryRow>(
        &state.pool.get().await?,
        r#"
        SELECT d.id, d.route_id, d.drone_id, d.cargo, d.status, d.scheduled_at,
               d.started_at, d.completed_at, d.notes, d.created_at, d.updated_at,
               r.name AS route_name, r.distance_m AS route_distance_m,
               r.estimated_duration_s
        FROM neurovector.deliveries d
        JOIN neurovector.routes r ON r.id = d.route_id
        WHERE d.id = $1
        "#,
        &[&(id)],
    )
    .await?
    .ok_or(AppError::NotFound)?;

    Ok(Json(DeliveryDetail {
        delivery: row.delivery,
        route_name: row.route_name,
        route_distance_m: row.route_distance_m,
        estimated_duration_s: row.estimated_duration_s,
    }))
}

#[derive(Debug)]
struct DeliveryRow {
    delivery: Delivery,
    route_name: String,
    route_distance_m: f64,
    estimated_duration_s: f64,
}
impl crate::db::FromRow for DeliveryRow {
    fn from_row(row: &tokio_postgres::Row) -> Result<Self, tokio_postgres::Error> {
        Ok(Self {
            delivery: crate::db::FromRow::from_row(row)?,
            route_name: row.try_get("route_name")?,
            route_distance_m: row.try_get("route_distance_m")?,
            estimated_duration_s: row.try_get("estimated_duration_s")?,
        })
    }
}

async fn create_delivery(
    State(state): State<AppState>,
    Json(body): Json<CreateDelivery>,
) -> Result<(StatusCode, Json<Delivery>), AppError> {
    if body.drone_id.trim().is_empty() {
        return Err(AppError::BadRequest("drone_id is required".into()));
    }
    if body.cargo.trim().is_empty() {
        return Err(AppError::BadRequest("cargo is required".into()));
    }

    let route = fetch_route(&state.pool, body.route_id).await?;
    if let Some(route_drone_id) = &route.drone_id
        && route_drone_id != &body.drone_id
    {
        return Err(AppError::Conflict(format!(
            "route is assigned to drone {route_drone_id}"
        )));
    }

    let overlap = crate::db::scalar_one::<bool>(
        &state.pool.get().await?,
        r#"
        SELECT EXISTS(SELECT 1
        FROM neurovector.deliveries
        WHERE drone_id = $1
          AND status IN ('scheduled', 'dispatched')
          AND scheduled_at > $2::timestamptz - interval '10 minutes'
          AND scheduled_at < $2::timestamptz + interval '10 minutes')
        "#,
        &[&(&body.drone_id), &(body.scheduled_at)],
    )
    .await?;

    if overlap {
        return Err(AppError::Conflict(
            "another delivery for this drone is scheduled within 10 minutes".into(),
        ));
    }

    let delivery = crate::db::fetch_one::<Delivery>(
        &state.pool.get().await?,
        r#"
        INSERT INTO neurovector.deliveries (route_id, drone_id, cargo, scheduled_at, notes)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING id, route_id, drone_id, cargo, status, scheduled_at, started_at,
                  completed_at, notes, created_at, updated_at
        "#,
        &[
            &(body.route_id),
            &(&body.drone_id),
            &(&body.cargo),
            &(body.scheduled_at),
            &(body.notes.as_deref()),
        ],
    )
    .await?;

    Ok((StatusCode::CREATED, Json(delivery)))
}

async fn update_status(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateStatus>,
) -> Result<Json<Delivery>, AppError> {
    let current = crate::db::fetch_optional::<Delivery>(
        &state.pool.get().await?,
        r#"
        SELECT id, route_id, drone_id, cargo, status, scheduled_at, started_at,
               completed_at, notes, created_at, updated_at
        FROM neurovector.deliveries
        WHERE id = $1
        "#,
        &[&(id)],
    )
    .await?
    .ok_or(AppError::NotFound)?;

    validate_transition(&current.status, &body.status)?;
    if matches!(body.status.as_str(), "dispatched" | "in_flight") {
        crate::mission::verify_route_for_dispatch(&state, current.route_id, &current.drone_id, id)
            .await?;
        validate_registered_dispatch_readiness(&state, id).await?;
        validate_delivery_order_readiness(&state, id).await?;
    }

    let mut connection = state.pool.get().await?;
    let tx = connection.transaction().await?;
    if matches!(body.status.as_str(), "dispatched" | "in_flight") {
        let telemetry_ttl_seconds = state.telemetry_online_ttl_seconds.to_string();
        crate::db::scalar_one::<String>(
            &tx,
            "SELECT set_config('orca.telemetry_online_ttl_seconds', $1, true)",
            &[&(telemetry_ttl_seconds)],
        )
        .await?;
    }
    let delivery = crate::db::fetch_optional::<Delivery>(
        &tx,
        r#"
        UPDATE neurovector.deliveries
        SET status = $2,
            started_at = CASE
                WHEN $2 = 'in_flight' AND started_at IS NULL THEN now()
                ELSE started_at
            END,
            completed_at = CASE
                WHEN $2 IN ('completed', 'failed', 'cancelled')
                    THEN COALESCE(completed_at, now())
                ELSE completed_at
            END,
            notes = COALESCE($3, notes),
            updated_at = now()
        WHERE id = $1 AND status = $4
        RETURNING id, route_id, drone_id, cargo, status, scheduled_at, started_at,
                  completed_at, notes, created_at, updated_at
        "#,
        &[
            &(id),
            &(&body.status),
            &(body.notes.as_deref()),
            &(&current.status),
        ],
    )
    .await
    .map_err(map_delivery_status_error)?
    .ok_or_else(|| AppError::Conflict("delivery status changed; reload and retry".into()))?;
    tx.commit().await?;
    drop(connection);

    Ok(Json(delivery))
}

async fn validate_delivery_order_readiness(
    state: &AppState,
    delivery_id: Uuid,
) -> Result<(), AppError> {
    let has_unready_order = crate::db::scalar_one::<bool>(
        &state.pool.get().await?,
        r#"
        SELECT EXISTS (
            SELECT 1
            FROM neurovector.delivery_stops s
            JOIN neurovector.orders o ON o.id = s.order_id
            WHERE s.delivery_id = $1
              AND s.status IN ('scheduled', 'arrived')
              AND o.status NOT IN ('assigned', 'in_transit', 'delivered')
        )
        "#,
        &[&(delivery_id)],
    )
    .await?;
    if has_unready_order {
        return Err(AppError::Conflict(
            "all scheduled delivery orders must be ready before dispatch".into(),
        ));
    }
    Ok(())
}

fn map_delivery_status_error(error: tokio_postgres::Error) -> AppError {
    let is_readiness_conflict = error.as_db_error().is_some_and(|database_error| {
        is_delivery_readiness_guard_message(database_error.message())
    });
    if is_readiness_conflict {
        AppError::Conflict("all scheduled delivery orders must be ready before dispatch".into())
    } else {
        AppError::Database(error)
    }
}

fn is_delivery_readiness_guard_message(message: &str) -> bool {
    message == DELIVERY_ORDER_READINESS_GUARD_MESSAGE
}

#[derive(Debug)]
struct DispatchReadinessRow {
    schedule_source: String,
    fleet_active: Option<bool>,
    capacity_grams: Option<i32>,
    telemetry_ts: Option<DateTime<Utc>>,
    telemetry_received_at: Option<DateTime<Utc>>,
    telemetry_status: Option<String>,
    measured_weight_grams: Option<i32>,
    measured_at: Option<DateTime<Utc>>,
}
crate::db::row!(DispatchReadinessRow {
    schedule_source,
    fleet_active,
    capacity_grams,
    telemetry_ts,
    telemetry_received_at,
    telemetry_status,
    measured_weight_grams,
    measured_at
});

async fn validate_registered_dispatch_readiness(
    state: &AppState,
    delivery_id: Uuid,
) -> Result<(), AppError> {
    let row = crate::db::fetch_optional::<DispatchReadinessRow>(
        &state.pool.get().await?,
        r#"
        SELECT d.schedule_source, f.active AS fleet_active, f.capacity_grams,
               telemetry.ts AS telemetry_ts,
               telemetry.received_at AS telemetry_received_at,
               telemetry.status AS telemetry_status,
               measurement.weight_grams AS measured_weight_grams,
               measurement.measured_at
        FROM neurovector.deliveries d
        LEFT JOIN neurovector.delivery_fleet f ON f.drone_id = d.drone_id
        LEFT JOIN LATERAL (
            SELECT t.ts, t.received_at, t.status
            FROM neurovector.current_drone_telemetry t
            WHERE t.drone_id = d.drone_id
            ORDER BY t.received_at DESC, t.ts DESC, t.id DESC
            LIMIT 1
        ) telemetry ON true
        LEFT JOIN LATERAL (
            SELECT m.weight_grams, m.measured_at
            FROM neurovector.delivery_weight_measurements m
            WHERE m.delivery_id = d.id
            ORDER BY m.measured_at DESC, m.id DESC
            LIMIT 1
        ) measurement ON true
        WHERE d.id = $1
        "#,
        &[&(delivery_id)],
    )
    .await?
    .ok_or(AppError::NotFound)?;

    let Some(capacity_grams) = row.capacity_grams else {
        if row.schedule_source == "automatic" {
            return Err(AppError::Conflict(
                "automatic delivery requires an active registered fleet drone".into(),
            ));
        }
        return Ok(());
    };
    if row.fleet_active != Some(true) {
        return Err(AppError::Conflict(
            "registered fleet drone is inactive".into(),
        ));
    }

    let ttl_seconds = i64::try_from(state.telemetry_online_ttl_seconds).map_err(|_| {
        AppError::ServiceUnavailable("telemetry freshness setting is out of range".into())
    })?;
    let max_age = Duration::seconds(ttl_seconds);
    let now = Utc::now();
    let fresh_telemetry =
        telemetry_is_fresh(row.telemetry_ts, row.telemetry_received_at, now, max_age)
            && !matches!(
                row.telemetry_status
                    .as_deref()
                    .map(str::to_ascii_lowercase)
                    .as_deref(),
                Some("failed" | "aborted")
            );
    if !fresh_telemetry {
        return Err(AppError::Conflict(
            "registered fleet drone requires fresh live telemetry".into(),
        ));
    }

    if row.schedule_source == "automatic"
        && !payload_measurement_is_recent_positive(row.measured_weight_grams, row.measured_at, now)
    {
        return Err(AppError::Conflict(
            "automatic delivery requires a recent positive measured payload".into(),
        ));
    }
    if row
        .measured_weight_grams
        .is_some_and(|weight| weight > capacity_grams)
    {
        return Err(AppError::Conflict(
            "latest measured payload exceeds current drone capacity".into(),
        ));
    }
    Ok(())
}

fn timestamp_is_recent(timestamp: DateTime<Utc>, now: DateTime<Utc>, max_age: Duration) -> bool {
    let age = now.signed_duration_since(timestamp);
    age >= Duration::seconds(-MAX_FUTURE_CLOCK_SKEW_SECONDS) && age <= max_age
}

fn telemetry_is_fresh(
    device_timestamp: Option<DateTime<Utc>>,
    received_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    max_age: Duration,
) -> bool {
    device_timestamp.is_some_and(|at| timestamp_is_recent(at, now, max_age))
        && received_at.is_some_and(|at| timestamp_is_recent(at, now, max_age))
}

fn payload_measurement_is_recent_positive(
    weight_grams: Option<i32>,
    measured_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> bool {
    weight_grams.is_some_and(|weight| weight > 0)
        && measured_at.is_some_and(|at| {
            timestamp_is_recent(at, now, Duration::seconds(MAX_MEASUREMENT_AGE_SECONDS))
        })
}

fn validate_transition(current: &str, next: &str) -> Result<(), AppError> {
    if current == next {
        return Err(AppError::Conflict(format!(
            "delivery is already in status {current}"
        )));
    }
    let allowed = match current {
        "scheduled" => matches!(next, "dispatched" | "cancelled"),
        "dispatched" => matches!(next, "in_flight" | "failed" | "cancelled"),
        "in_flight" => matches!(next, "completed" | "failed"),
        "completed" | "failed" | "cancelled" => false,
        _ => {
            return Err(AppError::BadRequest(format!(
                "unknown current status {current}"
            )));
        }
    };
    if !allowed {
        return Err(AppError::Conflict(format!(
            "cannot transition delivery from {current} to {next}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telemetry_requires_recent_device_and_receive_timestamps() {
        // Given one recently received but device-stale sample, When checking readiness, Then the registered drone remains offline.
        let now = Utc::now();
        let max_age = Duration::seconds(120);
        assert!(telemetry_is_fresh(Some(now), Some(now), now, max_age));
        assert!(!telemetry_is_fresh(
            Some(now - Duration::hours(1)),
            Some(now),
            now,
            max_age,
        ));
        assert!(!telemetry_is_fresh(
            Some(now),
            Some(now - Duration::hours(1)),
            now,
            max_age,
        ));
    }

    #[test]
    fn payload_measurement_requires_positive_recent_weight() {
        // Given missing, zero, stale, or current weights, When checking automatic dispatch, Then only a recent positive weight is accepted.
        let now = Utc::now();
        assert!(!payload_measurement_is_recent_positive(None, None, now));
        assert!(!payload_measurement_is_recent_positive(
            Some(0),
            Some(now),
            now,
        ));
        assert!(!payload_measurement_is_recent_positive(
            Some(100),
            Some(now - Duration::seconds(MAX_MEASUREMENT_AGE_SECONDS + 1)),
            now,
        ));
        assert!(payload_measurement_is_recent_positive(
            Some(100),
            Some(now),
            now,
        ));
    }

    #[test]
    fn delivery_transition_table_keeps_existing_mission_lifecycle() {
        // Given scheduled and in-flight deliveries, When checking transitions, Then allowed and terminal edges remain stable.
        assert!(validate_transition("scheduled", "dispatched").is_ok());
        assert!(validate_transition("dispatched", "in_flight").is_ok());
        assert!(validate_transition("in_flight", "completed").is_ok());
        assert!(validate_transition("completed", "cancelled").is_err());
    }

    #[test]
    fn only_the_dispatch_readiness_trigger_maps_to_conflict() {
        // Given the database dispatch guard or an unrelated database failure, When mapping the status result, Then only the known readiness guard is a 409 conflict.
        assert!(is_delivery_readiness_guard_message(
            DELIVERY_ORDER_READINESS_GUARD_MESSAGE
        ));
        assert!(!is_delivery_readiness_guard_message(
            "registered fleet drone requires fresh live telemetry"
        ));
    }
}
