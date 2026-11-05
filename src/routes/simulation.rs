//! Minimal order feed for the isolated school simulator, without customer information.
use crate::{error::AppError, state::AppState};
use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub fn router() -> Router<AppState> {
    Router::new().route("/simulation/orders", get(orders))
}

#[derive(Deserialize)]
pub struct Cursor {
    after: DateTime<Utc>,
    after_id: Uuid,
}

#[derive(Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct DispatchOrder {
    id: Uuid,
    status: String,
    created_at: DateTime<Utc>,
    destination_id: Option<String>,
}

#[utoipa::path(get,path="/api/v1/simulation/orders",tag="simulation",params(("after"=DateTime<Utc>,Query,description="UTC 주문 생성 시각 커서"),("after_id"=Uuid,Query,description="동일 생성 시각의 주문 ID 커서")),responses((status=200,description="학교 시뮬레이터용 최소 주문 목록, 최대 100개",body=Vec<DispatchOrder>),(status=404,description="학교 시뮬레이션 비활성화")))]
pub(crate) async fn orders(
    State(state): State<AppState>,
    Query(cursor): Query<Cursor>,
) -> Result<Json<Vec<DispatchOrder>>, AppError> {
    if std::env::var("SCHOOL_SIM_ENABLED").as_deref() != Ok("1") {
        return Err(AppError::NotFound);
    }
    let orders = sqlx::query_as::<_, DispatchOrder>(
        "SELECT id, status, created_at, CASE regexp_replace(COALESCE(delivery_detail, ''), '[[:space:]]', '', 'g') WHEN '1층현관' THEN 'lobby' WHEN '체육관앞' THEN 'gym' WHEN '본관후문' THEN 'back' WHEN '운동장입구' THEN 'playground' ELSE NULL END AS destination_id FROM neurovector.orders WHERE (created_at, id) > ($1, $2) ORDER BY created_at, id LIMIT 100"
    ).bind(cursor.after).bind(cursor.after_id).fetch_all(&state.pool).await?;
    Ok(Json(orders))
}
