use std::collections::HashMap;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, patch},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{error::AppError, routes::route_plans::fetch_route, state::AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_deliveries).post(create_delivery))
        .route("/{id}", get(get_delivery))
        .route("/{id}/status", patch(update_status))
}

#[derive(Debug, Serialize, sqlx::FromRow)]
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
    let deliveries = sqlx::query_as::<_, Delivery>(
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
    )
    .bind(params.status.as_deref())
    .bind(params.drone_id.as_deref())
    .bind(params.from)
    .bind(params.to)
    .bind(limit)
    .fetch_all(&state.pool)
    .await?;

    Ok(Json(deliveries))
}

async fn get_delivery(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<DeliveryDetail>, AppError> {
    let row = sqlx::query_as::<_, DeliveryRow>(
        r#"
        SELECT d.id, d.route_id, d.drone_id, d.cargo, d.status, d.scheduled_at,
               d.started_at, d.completed_at, d.notes, d.created_at, d.updated_at,
               r.name AS route_name, r.distance_m AS route_distance_m,
               r.estimated_duration_s
        FROM neurovector.deliveries d
        JOIN neurovector.routes r ON r.id = d.route_id
        WHERE d.id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::NotFound)?;

    Ok(Json(DeliveryDetail {
        delivery: row.delivery,
        route_name: row.route_name,
        route_distance_m: row.route_distance_m,
        estimated_duration_s: row.estimated_duration_s,
    }))
}

#[derive(Debug, sqlx::FromRow)]
struct DeliveryRow {
    #[sqlx(flatten)]
    delivery: Delivery,
    route_name: String,
    route_distance_m: f64,
    estimated_duration_s: f64,
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

    let overlap = sqlx::query_scalar::<_, bool>(
        r#"
        SELECT EXISTS(SELECT 1
        FROM neurovector.deliveries
        WHERE drone_id = $1
          AND status IN ('scheduled', 'dispatched')
          AND scheduled_at > $2::timestamptz - interval '10 minutes'
          AND scheduled_at < $2::timestamptz + interval '10 minutes')
        "#,
    )
    .bind(&body.drone_id)
    .bind(body.scheduled_at)
    .fetch_one(&state.pool)
    .await?;

    if overlap {
        return Err(AppError::Conflict(
            "another delivery for this drone is scheduled within 10 minutes".into(),
        ));
    }

    let delivery = sqlx::query_as::<_, Delivery>(
        r#"
        INSERT INTO neurovector.deliveries (route_id, drone_id, cargo, scheduled_at, notes)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING id, route_id, drone_id, cargo, status, scheduled_at, started_at,
                  completed_at, notes, created_at, updated_at
        "#,
    )
    .bind(body.route_id)
    .bind(&body.drone_id)
    .bind(&body.cargo)
    .bind(body.scheduled_at)
    .bind(body.notes.as_deref())
    .fetch_one(&state.pool)
    .await?;

    Ok((StatusCode::CREATED, Json(delivery)))
}

async fn update_status(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateStatus>,
) -> Result<Json<Delivery>, AppError> {
    let current = sqlx::query_as::<_, Delivery>(
        r#"
        SELECT id, route_id, drone_id, cargo, status, scheduled_at, started_at,
               completed_at, notes, created_at, updated_at
        FROM neurovector.deliveries
        WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::NotFound)?;

    validate_transition(&current.status, &body.status)?;
    if matches!(body.status.as_str(), "dispatched" | "in_flight") {
        crate::mission::verify_route_for_dispatch(&state, current.route_id, &current.drone_id)
            .await?;
    }

    let delivery = sqlx::query_as::<_, Delivery>(
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
    )
    .bind(id)
    .bind(&body.status)
    .bind(body.notes.as_deref())
    .bind(&current.status)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::Conflict("delivery status changed; reload and retry".into()))?;

    Ok(Json(delivery))
}

fn validate_transition(current: &str, next: &str) -> Result<(), AppError> {
    let allowed: HashMap<&str, &[&str]> = HashMap::from([
        ("scheduled", &["dispatched", "cancelled"][..]),
        ("dispatched", &["in_flight", "failed", "cancelled"][..]),
        ("in_flight", &["completed", "failed"][..]),
        ("completed", &[][..]),
        ("failed", &[][..]),
        ("cancelled", &[][..]),
    ]);

    if current == next {
        return Err(AppError::Conflict(format!(
            "delivery is already in status {current}"
        )));
    }
    let Some(allowed_next) = allowed.get(current) else {
        return Err(AppError::BadRequest(format!(
            "unknown current status {current}"
        )));
    };
    if !allowed_next.contains(&next) {
        return Err(AppError::Conflict(format!(
            "cannot transition delivery from {current} to {next}"
        )));
    }
    Ok(())
}
