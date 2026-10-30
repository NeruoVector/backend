use axum::{
    Extension, Json, Router,
    extract::{Path, State},
    middleware,
    routing::{get, post},
};
use deadpool_postgres::Transaction;
use uuid::Uuid;

use super::{HandoffRecord, MAX_PIN_ATTEMPTS, handoff_allowed, validate_pin, verify_pin};
use crate::{
    customer_auth::{self, Customer},
    error::AppError,
    order_queue,
    routes::orders::{self, Order},
    state::AppState,
};

#[derive(serde::Deserialize)]
struct HandoffRequest {
    pin: String,
}

pub(super) fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/account/orders", get(account_orders))
        .route("/orders/{id}/handoff", post(complete_handoff))
        .route_layer(middleware::from_fn_with_state(
            state,
            customer_auth::require_customer,
        ))
        .route_layer(middleware::from_fn(customer_auth::require_same_origin))
}

async fn account_orders(
    State(state): State<AppState>,
    Extension(customer): Extension<Customer>,
) -> Result<Json<Vec<Order>>, AppError> {
    let orders = crate::db::fetch_all::<Order>(
        &state.pool.get().await?,
        r#"
        SELECT o.id, o.order_number, o.customer_name, o.customer_phone, o.status,
               o.total_amount, o.currency, o.delivery_address, o.delivery_detail,
               o.latitude, o.longitude, o.notes, o.delivery_id, o.stock_restored_at,
               o.created_at, o.updated_at
        FROM neurovector.orders o
        WHERE o.customer_id = $1
        ORDER BY o.created_at DESC
        "#,
        &[&(customer.id)],
    )
    .await?;
    Ok(Json(orders))
}

async fn complete_handoff(
    State(state): State<AppState>,
    Extension(customer): Extension<Customer>,
    Path(order_id): Path<Uuid>,
    Json(body): Json<HandoffRequest>,
) -> Result<Json<Order>, AppError> {
    validate_pin(&body.pin)?;
    let snapshot = crate::db::fetch_optional::<HandoffRecord>(&state.pool.get().await?, "SELECT customer_id, pin_hash, attempts, consumed_at FROM neurovector.order_handoffs WHERE order_id = $1", &[&(order_id)])
    .await?
    .ok_or(AppError::NotFound)?;
    if snapshot.customer_id != customer.id {
        return Err(AppError::NotFound);
    }
    if snapshot.attempts >= MAX_PIN_ATTEMPTS || snapshot.consumed_at.is_some() {
        return Err(AppError::Conflict(
            "handoff PIN is no longer available".into(),
        ));
    }
    let (current_status, stop_status, expected_delivery_id) =
        crate::db::fetch_optional::<(String, Option<String>, Option<Uuid>)>(
            &state.pool.get().await?,
            r#"
        SELECT o.status, s.status, o.delivery_id
        FROM neurovector.orders o
        LEFT JOIN neurovector.delivery_stops s ON s.order_id = o.id
        WHERE o.id = $1
        "#,
            &[&(order_id)],
        )
        .await?
        .ok_or(AppError::NotFound)?;
    if !handoff_allowed(&current_status, stop_status.as_deref()) {
        return Err(AppError::Conflict(
            "order has not arrived for handoff".into(),
        ));
    }
    let matches = verify_pin(snapshot.pin_hash, body.pin).await?;
    let mut connection = state.pool.get().await?;
    let mut tx = connection.transaction().await?;
    let Some(expected_delivery_id) = expected_delivery_id else {
        return Err(AppError::Conflict("delivery assignment required".into()));
    };
    if !crate::routes::tracking::arrival::lock_delivery_pose(&tx, expected_delivery_id).await? {
        return Err(AppError::Conflict(
            "delivery assignment changed during handoff".into(),
        ));
    }
    let (status, delivery_id) = lock_order(&mut tx, order_id).await?;
    if delivery_id != Some(expected_delivery_id) {
        return Err(AppError::Conflict(
            "delivery assignment changed during handoff".into(),
        ));
    }
    let stop = crate::db::fetch_optional::<(Uuid, String)>(
        &tx,
        "SELECT id, status FROM neurovector.delivery_stops WHERE order_id = $1 AND delivery_id = $2 FOR UPDATE",
        &[&(order_id), &(expected_delivery_id)],
    )
    .await?;
    let Some((stop_id, stop_status)) = stop else {
        return Err(AppError::Conflict(
            "order has not arrived for handoff".into(),
        ));
    };
    if !handoff_allowed(&status, Some(&stop_status)) {
        return Err(AppError::Conflict(
            "order has not arrived for handoff".into(),
        ));
    }
    if !crate::routes::tracking::arrival::handoff_pose_is_valid(&tx, order_id).await? {
        return Err(AppError::Conflict(
            "current stable handoff pose required".into(),
        ));
    }
    let current = lock_handoff(&mut tx, order_id).await?;
    if current.customer_id != customer.id || current.consumed_at.is_some() {
        return Err(AppError::Conflict(
            "handoff PIN is no longer available".into(),
        ));
    }
    if current.attempts >= MAX_PIN_ATTEMPTS {
        return Err(AppError::Conflict("handoff PIN attempts exhausted".into()));
    }
    if !matches {
        crate::db::execute(&tx, "UPDATE neurovector.order_handoffs SET attempts = attempts + 1 WHERE order_id = $1 AND attempts < 5", &[&(order_id)])
            .await?;
        tx.commit().await?;
        drop(connection);
        return Err(AppError::Unauthorized("handoff PIN is incorrect".into()));
    }

    crate::db::execute(&tx, "UPDATE neurovector.order_handoffs SET consumed_at = now() WHERE order_id = $1 AND consumed_at IS NULL AND attempts < 5", &[&(order_id)])
        .await?;
    let order = crate::db::fetch_one::<Order>(
        &tx,
        r#"
        UPDATE neurovector.orders
        SET status = 'delivered', updated_at = now()
        WHERE id = $1 AND status = $2
        RETURNING id, order_number, customer_name, customer_phone, status, total_amount,
                  currency, delivery_address, delivery_detail, latitude, longitude, notes,
                  delivery_id, stock_restored_at, created_at, updated_at
        "#,
        &[&(order_id), &(status)],
    )
    .await
    .map_err(orders::map_handoff_guard)?;
    // The sync trigger uses transaction-start now(); record the actual boundary
    // while the advisory lock still excludes newer telemetry ingestion.
    let changed = crate::db::execute(&tx, "UPDATE neurovector.delivery_stops SET completed_at = clock_timestamp() WHERE id = $1 AND order_id = $2 AND delivery_id = $3 AND status = 'completed'", &[&stop_id, &order_id, &expected_delivery_id]).await?;
    if changed != 1 {
        return Err(AppError::Conflict(
            "delivery stop changed during handoff".into(),
        ));
    }
    complete_delivery_if_finished(&mut tx, expected_delivery_id).await?;
    tx.commit().await?;
    drop(connection);
    orders::invalidate_order_cache(&state).await;
    order_queue::publish_order_event(
        &state,
        "order.handoff_completed",
        order.id,
        None,
        serde_json::json!({"order_number": &order.order_number, "status": &order.status}),
    )
    .await;
    Ok(Json(order))
}

async fn lock_order(
    tx: &mut Transaction<'_>,
    order_id: Uuid,
) -> Result<(String, Option<Uuid>), AppError> {
    crate::db::fetch_optional::<(String, Option<Uuid>)>(
        &*tx,
        "SELECT status, delivery_id FROM neurovector.orders WHERE id = $1 FOR UPDATE",
        &[&(order_id)],
    )
    .await?
    .ok_or(AppError::NotFound)
}

async fn lock_handoff(tx: &mut Transaction<'_>, order_id: Uuid) -> Result<HandoffRecord, AppError> {
    crate::db::fetch_optional::<HandoffRecord>(&*tx, "SELECT customer_id, pin_hash, attempts, consumed_at FROM neurovector.order_handoffs WHERE order_id = $1 FOR UPDATE", &[&(order_id)])
    .await?
    .ok_or(AppError::NotFound)
}

async fn complete_delivery_if_finished(
    tx: &mut Transaction<'_>,
    delivery_id: Uuid,
) -> Result<(), AppError> {
    let unfinished = crate::db::scalar_one::<bool>(&*tx, "SELECT EXISTS (SELECT 1 FROM neurovector.orders WHERE delivery_id = $1 AND status NOT IN ('delivered', 'cancelled'))", &[&(delivery_id)])
    .await?;
    if !unfinished {
        crate::db::execute(&*tx, "UPDATE neurovector.deliveries SET status = 'completed', completed_at = COALESCE(completed_at, now()), updated_at = now() WHERE id = $1", &[&(delivery_id)])
        .await?;
    }
    Ok(())
}
