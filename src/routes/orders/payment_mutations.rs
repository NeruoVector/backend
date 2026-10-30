use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use serde_json::json;
use uuid::Uuid;

use crate::{error::AppError, order_queue, state::AppState};

use super::{
    access, invalidate_order_cache,
    models::{CreatePayment, Payment},
    persistence::{fetch_order_for_update, lock_delivery, order_delivery_id, restore_stock},
};

#[utoipa::path(
    post,
    path = "/api/v1/orders/{id}/payments",
    params(("id" = Uuid, Path, description = "주문 ID")),
    request_body = CreatePayment,
    responses(
        (status = 201, description = "더미 결제 성공", body = Payment),
        (status = 402, description = "더미 결제 실패", body = Payment),
        (status = 409, description = "결제 불가 상태")
    ),
    tag = "orders"
)]
pub(crate) async fn pay_order(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<CreatePayment>,
) -> Result<(StatusCode, Json<Payment>), AppError> {
    access::authorize_order(&state.pool, &headers, id).await?;
    let method = body.method.unwrap_or_else(|| "dummy_card".to_string());
    let simulate = body.simulate.unwrap_or_else(|| "success".to_string());
    if !matches!(simulate.as_str(), "success" | "failure") {
        return Err(AppError::BadRequest(
            "simulate must be 'success' or 'failure'".into(),
        ));
    }

    let mut connection = state.pool.get().await?;
    let mut tx = connection.transaction().await?;
    let order = fetch_order_for_update(&mut tx, id).await?;

    if order.status != "pending_payment" {
        return Err(AppError::Conflict(format!(
            "order is not awaiting payment (status: {})",
            order.status
        )));
    }
    let already_captured = crate::db::scalar_one::<bool>(
        &tx,
        r#"
        SELECT EXISTS(
            SELECT 1 FROM neurovector.payments
            WHERE order_id = $1 AND status IN ('authorized', 'captured')
        )
        "#,
        &[&(id)],
    )
    .await?;
    if already_captured {
        return Err(AppError::Conflict("order already has a payment".into()));
    }

    let (status, failure_reason, transaction_id) = if simulate == "failure" {
        (
            "failed",
            Some("dummy payment declined".to_string()),
            None::<String>,
        )
    } else {
        ("captured", None, Some(format!("DUMMY-{}", Uuid::new_v4())))
    };

    let payment = crate::db::fetch_one::<Payment>(
        &tx,
        r#"
        INSERT INTO neurovector.payments
            (order_id, provider, method, status, amount, currency, transaction_id, failure_reason)
        VALUES ($1, 'dummy', $2, $3, $4, $5, $6, $7)
        RETURNING id, order_id, provider, method, status, amount, currency,
                  transaction_id, failure_reason, created_at, updated_at
        "#,
        &[
            &(order.id),
            &(&method),
            &(status),
            &(order.total_amount),
            &(&order.currency),
            &(transaction_id),
            &(failure_reason),
        ],
    )
    .await?;

    if status == "captured" {
        crate::db::execute(
            &tx,
            "UPDATE neurovector.orders SET status = 'paid', updated_at = now() WHERE id = $1",
            &[&(order.id)],
        )
        .await?;
    }

    tx.commit().await?;
    drop(connection);

    invalidate_order_cache(&state).await;
    let event_type = if status == "captured" {
        "payment.captured"
    } else {
        "payment.failed"
    };
    order_queue::publish_order_event(
        &state,
        event_type,
        payment.order_id,
        None,
        json!({
            "payment_id": payment.id,
            "status": &payment.status,
            "amount": payment.amount,
            "currency": &payment.currency,
            "transaction_id": &payment.transaction_id,
            "failure_reason": &payment.failure_reason,
        }),
    )
    .await;

    let code = if status == "captured" {
        StatusCode::CREATED
    } else {
        StatusCode::PAYMENT_REQUIRED
    };
    Ok((code, Json(payment)))
}

#[utoipa::path(
    post,
    path = "/api/v1/payments/{id}/refund",
    params(("id" = Uuid, Path, description = "결제 ID")),
    responses(
        (status = 200, description = "환불 처리", body = Payment),
        (status = 409, description = "환불 불가 상태")
    ),
    tag = "orders"
)]
pub(crate) async fn refund_payment(
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
    let expected_delivery_id = order_delivery_id(&state.pool, order_id).await?;
    let mut connection = state.pool.get().await?;
    let mut tx = connection.transaction().await?;
    if let Some(delivery_id) = expected_delivery_id {
        lock_delivery(&mut tx, delivery_id).await?;
    }
    let order = fetch_order_for_update(&mut tx, order_id).await?;
    if order.delivery_id != expected_delivery_id {
        return Err(AppError::Conflict(
            "delivery assignment changed during refund".into(),
        ));
    }
    let payment = crate::db::fetch_optional::<Payment>(
        &tx,
        r#"
        SELECT id, order_id, provider, method, status, amount, currency,
               transaction_id, failure_reason, created_at, updated_at
        FROM neurovector.payments
        WHERE id = $1
        FOR UPDATE
        "#,
        &[&(id)],
    )
    .await?
    .ok_or(AppError::NotFound)?;

    if payment.status != "captured" {
        return Err(AppError::Conflict(
            "only captured payments can be refunded".into(),
        ));
    }

    if payment.order_id != order.id {
        return Err(AppError::NotFound);
    }
    if order.stock_restored_at.is_none() && order.status != "delivered" {
        restore_stock(&mut tx, order.id).await?;
    }

    let payment = crate::db::fetch_one::<Payment>(
        &tx,
        r#"
        UPDATE neurovector.payments
        SET status = 'refunded', updated_at = now()
        WHERE id = $1
        RETURNING id, order_id, provider, method, status, amount, currency,
                  transaction_id, failure_reason, created_at, updated_at
        "#,
        &[&(id)],
    )
    .await?;

    if order.status != "delivered" && order.status != "cancelled" {
        crate::db::execute(
            &tx,
            r#"
            UPDATE neurovector.orders
            SET status = 'cancelled',
                stock_restored_at = COALESCE(stock_restored_at, now()),
                updated_at = now()
            WHERE id = $1
            "#,
            &[&(order.id)],
        )
        .await?;
    }

    tx.commit().await?;
    drop(connection);

    invalidate_order_cache(&state).await;
    order_queue::publish_order_event(
        &state,
        "payment.refunded",
        payment.order_id,
        None,
        json!({
            "payment_id": payment.id,
            "order_id": payment.order_id,
            "status": &payment.status,
            "amount": payment.amount,
            "currency": &payment.currency,
            "order_status": &order.status,
        }),
    )
    .await;

    Ok(Json(payment))
}
