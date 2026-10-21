use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::get,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    error::AppError,
    ingest,
    models::{CreateDetection, Detection},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_detections).post(create_detection))
        .route("/{id}", get(get_detection))
}

#[derive(Debug, Deserialize)]
pub struct ListParams {
    /// 조회할 최대 건수 (기본 50, 최대 200)
    #[serde(default)]
    pub limit: Option<i64>,
}

/// 장애물 감지 이벤트 목록 (최신순)
async fn list_detections(
    State(state): State<AppState>,
    Query(params): Query<ListParams>,
) -> Result<Json<Vec<Detection>>, AppError> {
    let limit = params.limit.unwrap_or(50).clamp(1, 200);

    let rows = sqlx::query_as::<_, Detection>(
        r#"
        SELECT id, drone_id, label, confidence, latitude, longitude, distance_m,
               priority_level, detected_at, created_at
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

/// 장애물 감지 이벤트 저장 + Kafka 발행 (HTTP/MQTT 공용 경로)
async fn create_detection(
    State(state): State<AppState>,
    Json(body): Json<CreateDetection>,
) -> Result<(StatusCode, Json<Detection>), AppError> {
    let detection = ingest::ingest_detection(&state, body).await?;
    Ok((StatusCode::CREATED, Json(detection)))
}

/// 장애물 감지 이벤트 단건 조회
async fn get_detection(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Detection>, AppError> {
    let detection = sqlx::query_as::<_, Detection>(
        r#"
        SELECT id, drone_id, label, confidence, latitude, longitude, distance_m,
               priority_level, detected_at, created_at
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
