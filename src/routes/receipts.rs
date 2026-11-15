use axum::{
    Json, Router,
    extract::{Path, State},
    http::HeaderMap,
    routing::post,
};
use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

use crate::{customer_auth, error::AppError, state::AppState};

pub fn router() -> Router<AppState> {
    Router::new().route("/orders/{id}/receipt", post(receive))
}

#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReceiptInput {
    confirmed: bool,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct Receipt {
    order_id: Uuid,
    received: bool,
}

#[derive(Deserialize)]
struct SchoolState {
    timestamp: f64,
    order_id: Uuid,
    armed: bool,
    mission: SchoolMission,
}

#[derive(Deserialize)]
struct SchoolMission {
    order_id: Uuid,
    phase: ReceiptPhase,
    arrived: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReceiptPhase {
    AwaitingReceipt,
    #[serde(other)]
    Other,
}

fn unavailable() -> AppError {
    AppError::Conflict("드론이 목적지에 착륙한 뒤 수령을 확인해 주세요.".into())
}

fn verify_landed(raw: &[u8], order_id: Uuid, now: f64) -> Result<(), AppError> {
    let state: SchoolState = serde_json::from_slice(raw).map_err(|_| unavailable())?;
    let age = now - state.timestamp;
    if !state.timestamp.is_finite()
        || !age.is_finite()
        || !(0.0..=5.0).contains(&age)
        || state.order_id != order_id
        || state.mission.order_id != order_id
        || state.armed
        || !state.mission.arrived
    {
        return Err(unavailable());
    }
    match state.mission.phase {
        ReceiptPhase::AwaitingReceipt => Ok(()),
        ReceiptPhase::Other => Err(unavailable()),
    }
}

async fn verify_school_state(order_id: Uuid) -> Result<(), AppError> {
    let path = std::env::var_os("SCHOOL_SIM_STATE_FILE")
        .unwrap_or_else(|| "/school-runtime/state.json".into());
    let raw = tokio::time::timeout(Duration::from_secs(1), tokio::fs::read(path))
        .await
        .map_err(|_| unavailable())?
        .map_err(|_| unavailable())?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| unavailable())?
        .as_secs_f64();
    verify_landed(&raw, order_id, now)
}

#[utoipa::path(post, path="/api/v1/orders/{id}/receipt", tag="orders", params(("id"=Uuid,Path,description="내 주문 ID")), request_body=ReceiptInput, responses((status=200,description="수령 확인 완료",body=Receipt),(status=400,description="수령 확인 필요"),(status=401,description="로그인 필요"),(status=404,description="소유한 주문 없음 또는 학교 시뮬레이션 비활성화"),(status=409,description="드론이 목적지에서 수령 대기 중이 아님")))]
pub(crate) async fn receive(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<ReceiptInput>,
) -> Result<Json<Receipt>, AppError> {
    if std::env::var("SCHOOL_SIM_ENABLED").as_deref() != Ok("1") {
        return Err(AppError::NotFound);
    }
    let user = customer_auth::current(&state, &headers)
        .await?
        .ok_or_else(|| AppError::Unauthorized("로그인이 필요합니다.".into()))?;
    let mut tx = state.pool.begin().await?;
    let _: Uuid = sqlx::query_scalar(
        "SELECT id FROM neurovector.orders WHERE id=$1 AND customer_user_id=$2 FOR SHARE",
    )
    .bind(id)
    .bind(user.id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    if !body.confirmed {
        return Err(AppError::BadRequest("수령 확인이 필요합니다.".into()));
    }
    let received: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM neurovector.order_receipts WHERE order_id=$1)",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    if !received {
        verify_school_state(id).await?;
        sqlx::query(
            "INSERT INTO neurovector.order_receipts(order_id) VALUES ($1) ON CONFLICT (order_id) DO NOTHING",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(Json(Receipt {
        order_id: id,
        received: true,
    }))
}

#[cfg(test)]
mod tests;
