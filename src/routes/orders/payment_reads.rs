use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use uuid::Uuid;

use crate::{error::AppError, state::AppState};

use super::{access, models::Payment};

#[utoipa::path(
    get,
    path = "/api/v1/orders/{id}/payments",
    params(("id" = Uuid, Path, description = "주문 ID")),
    responses((status = 200, description = "결제 목록", body = Vec<Payment>)),
    tag = "orders"
)]
pub(crate) async fn list_order_payments(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Json<Vec<Payment>>, AppError> {
    access::authorize_order(&state.pool, &headers, id).await?;
    let payments = crate::db::fetch_all::<Payment>(
        &state.pool.get().await?,
        r#"
        SELECT id, order_id, provider, method, status, amount, currency,
               transaction_id, failure_reason, created_at, updated_at
        FROM neurovector.payments
        WHERE order_id = $1
        ORDER BY created_at DESC
        "#,
        &[&(id)],
    )
    .await?;
    Ok(Json(payments))
}

#[utoipa::path(
    get,
    path = "/api/v1/payments/{id}",
    params(("id" = Uuid, Path, description = "결제 ID")),
    responses(
        (status = 200, description = "결제 상세", body = Payment),
        (status = 404, description = "결제 없음")
    ),
    tag = "orders"
)]
pub(crate) async fn get_payment(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Json<Payment>, AppError> {
    let order_id = crate::db::scalar_optional::<Uuid>(
        &state.pool.get().await?,
        "SELECT order_id FROM neurovector.payments WHERE id = $1",
        &[&(id)],
    )
    .await?
    .ok_or(AppError::NotFound)?;
    access::authorize_order(&state.pool, &headers, order_id).await?;
    let payment = crate::db::fetch_optional::<Payment>(
        &state.pool.get().await?,
        r#"
        SELECT id, order_id, provider, method, status, amount, currency,
               transaction_id, failure_reason, created_at, updated_at
        FROM neurovector.payments
        WHERE id = $1
        "#,
        &[&(id)],
    )
    .await?
    .ok_or(AppError::NotFound)?;
    Ok(Json(payment))
}
