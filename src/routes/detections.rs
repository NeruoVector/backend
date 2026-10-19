use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::get,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{error::AppError, events, state::AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_detections).post(create_detection))
        .route("/{id}", get(get_detection))
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Detection {
    pub id: Uuid,
    pub drone_id: String,
    pub label: String,
    pub confidence: f32,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub detected_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct ListParams {
    /// 조회할 최대 건수 (기본 50, 최대 200)
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct CreateDetection {
    pub drone_id: String,
    pub label: String,
    pub confidence: f32,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub detected_at: Option<DateTime<Utc>>,
}

/// 장애물 감지 이벤트 목록 (최신순)
async fn list_detections(
    State(state): State<AppState>,
    Query(params): Query<ListParams>,
) -> Result<Json<Vec<Detection>>, AppError> {
    let limit = params.limit.unwrap_or(50).clamp(1, 200);

    let rows = sqlx::query_as::<_, Detection>(
        r#"
        SELECT id, drone_id, label, confidence, latitude, longitude, detected_at, created_at
        FROM neurovector.detections
        ORDER BY detected_at DESC
        LIMIT $1
        "#,
    )
    .bind(limit)
    .fetch_all(&state.pool)
    .await?;

    Ok(Json(rows))
}

/// 장애물 감지 이벤트 저장 + Kafka 발행
async fn create_detection(
    State(state): State<AppState>,
    Json(body): Json<CreateDetection>,
) -> Result<(StatusCode, Json<Detection>), AppError> {
    if body.drone_id.trim().is_empty() {
        return Err(AppError::BadRequest("drone_id is required".into()));
    }
    if body.label.trim().is_empty() {
        return Err(AppError::BadRequest("label is required".into()));
    }
    if !(0.0..=1.0).contains(&body.confidence) {
        return Err(AppError::BadRequest(
            "confidence must be between 0 and 1".into(),
        ));
    }

    let detection = sqlx::query_as::<_, Detection>(
        r#"
        INSERT INTO neurovector.detections
            (drone_id, label, confidence, latitude, longitude, detected_at)
        VALUES ($1, $2, $3, $4, $5, COALESCE($6, now()))
        RETURNING id, drone_id, label, confidence, latitude, longitude, detected_at, created_at
        "#,
    )
    .bind(&body.drone_id)
    .bind(&body.label)
    .bind(body.confidence)
    .bind(body.latitude)
    .bind(body.longitude)
    .bind(body.detected_at)
    .fetch_one(&state.pool)
    .await?;

    let event = events::DetectionEvent {
        id: detection.id,
        drone_id: &detection.drone_id,
        label: &detection.label,
        confidence: detection.confidence,
        detected_at: detection.detected_at,
    };

    if let Err(err) =
        events::publish_detection(&state.kafka_brokers, &state.kafka_topic_events, &event).await
    {
        // 이벤트 발행 실패는 저장 자체를 되돌리지 않는다.
        tracing::warn!(error = %err, detection_id = %detection.id, "failed to publish detection event");
    }

    Ok((StatusCode::CREATED, Json(detection)))
}

/// 장애물 감지 이벤트 단건 조회
async fn get_detection(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Detection>, AppError> {
    let detection = sqlx::query_as::<_, Detection>(
        r#"
        SELECT id, drone_id, label, confidence, latitude, longitude, detected_at, created_at
        FROM neurovector.detections
        WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::NotFound)?;

    Ok(Json(detection))
}
