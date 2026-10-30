use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use serde_json::json;
use uuid::Uuid;

use crate::{error::AppError, order_queue, state::AppState};

use super::{
    access, invalidate_order_cache,
    models::{Order, UpdateOrderStatus},
    persistence::{
        fetch_order_for_update, lock_delivery, map_handoff_guard, order_delivery_id, restore_stock,
    },
    validation::validate_order_transition,
};

#[utoipa::path(
    patch,
    path = "/api/v1/orders/{id}/status",
    params(("id" = Uuid, Path, description = "주문 ID")),
    request_body = UpdateOrderStatus,
    responses(
        (status = 200, description = "상태 변경", body = Order),
        (status = 409, description = "허용되지 않은 상태 전이")
    ),
    tag = "orders"
)]
pub(crate) async fn update_order_status(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<UpdateOrderStatus>,
) -> Result<Json<Order>, AppError> {
    let actor = access::actor(&state.pool, &headers).await?;
    let expected_delivery_id = order_delivery_id(&state.pool, id).await?;
    let mut connection = state.pool.get().await?;
    let mut tx = connection.transaction().await?;
    if let Some(delivery_id) = expected_delivery_id {
        lock_delivery(&mut tx, delivery_id).await?;
    }
    let current = fetch_order_for_update(&mut tx, id).await?;
    if current.delivery_id != expected_delivery_id {
        return Err(AppError::Conflict(
            "delivery assignment changed during status update".into(),
        ));
    }
    let owner_id = crate::db::scalar_one::<Option<Uuid>>(
        &tx,
        "SELECT customer_id FROM neurovector.orders WHERE id = $1",
        &[&(id)],
    )
    .await?;
    match (owner_id, actor) {
        (Some(_), access::Actor::Anonymous) => {
            return Err(AppError::Unauthorized(
                "sign in to change this order".into(),
            ));
        }
        (Some(_), access::Actor::Customer(customer_id)) => {
            if owner_id == Some(customer_id) {
                if !access::customer_can_cancel(actor, owner_id, &current.status, &body.status) {
                    return Err(AppError::Unauthorized(
                        "customers may only cancel their pending orders".into(),
                    ));
                }
            } else if access::is_merchant_owner(&mut tx, id, customer_id).await? {
                if !access::merchant_can_transition(&current.status, &body.status) {
                    return Err(AppError::Unauthorized(
                        "merchant cannot perform this status transition".into(),
                    ));
                }
            } else {
                return Err(AppError::NotFound);
            }
        }
        (_, access::Actor::Admin | access::Actor::Anonymous | access::Actor::Customer(_)) => {}
    }
    validate_order_transition(&current.status, &body.status)?;

    let stock_restored = body.status == "cancelled" && current.stock_restored_at.is_none();
    if stock_restored {
        restore_stock(&mut tx, id).await?;
    }

    let order = crate::db::fetch_one::<Order>(
        &tx,
        r#"
        UPDATE neurovector.orders
        SET status = $2,
            stock_restored_at = CASE
                WHEN $2 = 'cancelled' AND stock_restored_at IS NULL THEN now()
                ELSE stock_restored_at
            END,
            updated_at = now()
        WHERE id = $1
        RETURNING id, order_number, customer_name, customer_phone, status, total_amount,
                  currency, delivery_address, delivery_detail, latitude, longitude,
                  notes, delivery_id, stock_restored_at, created_at, updated_at
        "#,
        &[&(id), &(&body.status)],
    )
    .await
    .map_err(map_handoff_guard)?;

    tx.commit().await?;
    drop(connection);

    invalidate_order_cache(&state).await;
    order_queue::publish_order_event(
        &state,
        "order.status_changed",
        order.id,
        None,
        json!({
            "order_number": &order.order_number,
            "previous_status": current.status,
            "status": &order.status,
            "stock_restored": stock_restored,
        }),
    )
    .await;

    Ok(Json(order))
}
