use deadpool_postgres::Transaction;
use uuid::Uuid;

use crate::{error::AppError, state::AppState};

use super::models::{Order, OrderItem, Payment};

pub(super) async fn fetch_order(state: &AppState, id: Uuid) -> Result<Order, AppError> {
    crate::db::fetch_optional::<Order>(
        &state.pool.get().await?,
        r#"
        SELECT id, order_number, customer_name, customer_phone, status, total_amount,
               currency, delivery_address, delivery_detail, latitude, longitude,
               notes, delivery_id, stock_restored_at, created_at, updated_at
        FROM neurovector.orders
        WHERE id = $1
        "#,
        &[&(id)],
    )
    .await?
    .ok_or(AppError::NotFound)
}

pub(super) async fn fetch_order_for_update(
    tx: &mut Transaction<'_>,
    id: Uuid,
) -> Result<Order, AppError> {
    crate::db::fetch_optional::<Order>(
        &*tx,
        r#"
        SELECT id, order_number, customer_name, customer_phone, status, total_amount,
               currency, delivery_address, delivery_detail, latitude, longitude,
               notes, delivery_id, stock_restored_at, created_at, updated_at
        FROM neurovector.orders
        WHERE id = $1
        FOR UPDATE
        "#,
        &[&(id)],
    )
    .await?
    .ok_or(AppError::NotFound)
}

pub(super) async fn order_delivery_id(
    pool: &crate::db::PgPool,
    id: Uuid,
) -> Result<Option<Uuid>, AppError> {
    crate::db::scalar_optional::<Option<Uuid>>(
        &pool.get().await?,
        "SELECT delivery_id FROM neurovector.orders WHERE id = $1",
        &[&(id)],
    )
    .await?
    .ok_or(AppError::NotFound)
}

pub(super) async fn lock_delivery(
    tx: &mut Transaction<'_>,
    delivery_id: Uuid,
) -> Result<(), AppError> {
    crate::db::scalar_optional::<Uuid>(
        &*tx,
        "SELECT id FROM neurovector.deliveries WHERE id = $1 FOR UPDATE",
        &[&(delivery_id)],
    )
    .await?
    .ok_or(AppError::NotFound)?;
    Ok(())
}

pub(super) async fn fetch_order_items(
    state: &AppState,
    order_id: Uuid,
) -> Result<Vec<OrderItem>, AppError> {
    let items = crate::db::fetch_all::<OrderItem>(
        &state.pool.get().await?,
        r#"
        SELECT id, order_id, item_id, quantity, unit_price, line_total, weight_grams, created_at
        FROM neurovector.order_items
        WHERE order_id = $1
        ORDER BY created_at ASC
        "#,
        &[&(order_id)],
    )
    .await?;
    Ok(items)
}

pub(super) async fn fetch_latest_payment(
    state: &AppState,
    order_id: Uuid,
) -> Result<Option<Payment>, AppError> {
    let payment = crate::db::fetch_optional::<Payment>(
        &state.pool.get().await?,
        r#"
        SELECT id, order_id, provider, method, status, amount, currency,
               transaction_id, failure_reason, created_at, updated_at
        FROM neurovector.payments
        WHERE order_id = $1
        ORDER BY created_at DESC
        LIMIT 1
        "#,
        &[&(order_id)],
    )
    .await?;
    Ok(payment)
}

pub(super) async fn restore_stock(
    tx: &mut Transaction<'_>,
    order_id: Uuid,
) -> Result<(), AppError> {
    let items = crate::db::fetch_all::<OrderItem>(
        &*tx,
        r#"
        SELECT id, order_id, item_id, quantity, unit_price, line_total, weight_grams, created_at
        FROM neurovector.order_items
        WHERE order_id = $1
        ORDER BY item_id
        "#,
        &[&(order_id)],
    )
    .await?;

    // Match order creation's inventory lock order during cancellation and refund.
    for item in items {
        crate::db::execute(&*tx, "UPDATE neurovector.inventory_items SET stock_quantity = stock_quantity + $2, updated_at = now() WHERE id = $1", &[&(item.item_id), &(item.quantity)])
        .await?;
    }
    Ok(())
}

pub(crate) fn map_handoff_guard(error: tokio_postgres::Error) -> AppError {
    if let Some(database) = error.as_db_error()
        && database.code().code() == "23514"
        && matches!(
            database.constraint(),
            Some("order_handoff_guard" | "delivery_handoff_guard")
        )
    {
        return AppError::Conflict("handoff PIN verification required".into());
    }
    AppError::Database(error)
}
