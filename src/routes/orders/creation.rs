use std::collections::HashMap;

use chrono::Utc;
use deadpool_postgres::Transaction;
use uuid::Uuid;

use crate::error::AppError;

use super::models::{CreateOrder, Order, OrderDetail, OrderItem, StockItem};

fn validate_order_currency(current: Option<&str>, item_currency: &str) -> Result<(), AppError> {
    if current.is_some_and(|currency| currency != item_currency) {
        return Err(AppError::BadRequest(
            "all order items must use the same currency".into(),
        ));
    }
    Ok(())
}

fn validate_pin_checkout_item(
    pin_requested: bool,
    current_merchant: Option<Uuid>,
    item_merchant: Option<Uuid>,
    weight_grams: Option<i32>,
) -> Result<Option<Uuid>, AppError> {
    if !pin_requested {
        return Ok(current_merchant);
    }
    let merchant = item_merchant.ok_or_else(|| {
        AppError::BadRequest("PIN checkout items must belong to one merchant".into())
    })?;
    if current_merchant.is_some_and(|current| current != merchant) {
        return Err(AppError::BadRequest(
            "PIN checkout items must belong to one merchant".into(),
        ));
    }
    if weight_grams.is_none_or(|weight| weight <= 0) {
        return Err(AppError::BadRequest(
            "PIN checkout items require a positive weight_grams value".into(),
        ));
    }
    Ok(Some(merchant))
}

/// 동기 API 와 Kafka worker 가 공유하는 주문 생성 트랜잭션.
pub(crate) async fn create_order_in_tx(
    tx: &mut Transaction<'_>,
    body: &CreateOrder,
) -> Result<OrderDetail, AppError> {
    if let Some(delivery_id) = body.delivery_id {
        let exists = crate::db::scalar_one::<bool>(
            &*tx,
            "SELECT EXISTS(SELECT 1 FROM neurovector.deliveries WHERE id = $1)",
            &[&(delivery_id)],
        )
        .await?;
        if !exists {
            return Err(AppError::BadRequest("delivery_id does not exist".into()));
        }
    }

    let item_ids: Vec<Uuid> = body.items.iter().map(|item| item.item_id).collect();
    // Every order acquires inventory locks in the same order, regardless of input order.
    let stock_items = crate::db::fetch_all::<StockItem>(
        &*tx,
        r#"
        SELECT i.id, i.name, i.unit_price, i.currency, i.stock_quantity, i.active,
               d.merchant_id, d.weight_grams
        FROM neurovector.inventory_items i
        LEFT JOIN neurovector.product_details d ON d.item_id = i.id
        WHERE i.id = ANY($1::uuid[])
        ORDER BY i.id
        FOR UPDATE OF i
        "#,
        &[&(&item_ids)],
    )
    .await?;
    let stock_by_id: HashMap<Uuid, StockItem> = stock_items
        .into_iter()
        .map(|item| (item.id, item))
        .collect();
    let mut total_amount = 0i64;
    let mut currency: Option<String> = None;
    let mut quantities = Vec::with_capacity(body.items.len());
    let mut unit_prices = Vec::with_capacity(body.items.len());
    let mut line_totals = Vec::with_capacity(body.items.len());
    let mut weight_snapshots = Vec::with_capacity(body.items.len());
    let mut checkout_merchant = None;

    // Validate in request order so errors and the returned item order stay consistent.
    for requested in &body.items {
        let item = stock_by_id
            .get(&requested.item_id)
            .ok_or_else(|| AppError::BadRequest("order item does not exist".into()))?;

        if !item.active {
            return Err(AppError::Conflict(format!(
                "item {} is not active",
                item.name
            )));
        }
        if item.stock_quantity < requested.quantity {
            return Err(AppError::Conflict(format!(
                "insufficient stock for {}: requested {}, available {}",
                item.name, requested.quantity, item.stock_quantity
            )));
        }
        validate_order_currency(currency.as_deref(), &item.currency)?;
        currency = Some(item.currency.clone());
        checkout_merchant = validate_pin_checkout_item(
            body.handoff_pin.is_some(),
            checkout_merchant,
            item.merchant_id,
            item.weight_grams,
        )?;

        let line_total = item
            .unit_price
            .checked_mul(requested.quantity as i64)
            .ok_or_else(|| AppError::BadRequest("order amount is too large".into()))?;
        total_amount = total_amount
            .checked_add(line_total)
            .ok_or_else(|| AppError::BadRequest("order amount is too large".into()))?;

        quantities.push(requested.quantity);
        unit_prices.push(item.unit_price);
        line_totals.push(line_total);
        weight_snapshots.push(item.weight_grams.unwrap_or(0));
    }

    // All quantities and amounts have been checked while holding the inventory locks.
    crate::db::execute(
        &*tx,
        r#"
        UPDATE neurovector.inventory_items AS inventory
        SET stock_quantity = inventory.stock_quantity - requested.quantity, updated_at = now()
        FROM UNNEST($1::uuid[], $2::int[]) AS requested(item_id, quantity)
        WHERE inventory.id = requested.item_id
        "#,
        &[&(&item_ids), &(&quantities)],
    )
    .await?;

    let currency = currency.unwrap_or_else(|| "KRW".to_string());
    let order_number = format!(
        "NV-{}-{}",
        Utc::now().format("%Y%m%d"),
        Uuid::new_v4().simple().to_string()[..8].to_uppercase()
    );

    let order = crate::db::fetch_one::<Order>(
        &*tx,
        r#"
        INSERT INTO neurovector.orders
            (order_number, customer_name, customer_phone, status, total_amount, currency,
             delivery_address, delivery_detail, latitude, longitude, notes, delivery_id,
             room_number, grade, classroom)
        VALUES ($1, $2, $3, 'pending_payment', $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
        RETURNING id, order_number, customer_name, customer_phone, status, total_amount,
                  currency, delivery_address, delivery_detail, latitude, longitude,
                  notes, delivery_id, stock_restored_at, created_at, updated_at
        "#,
        &[
            &(&order_number),
            &(body.customer_name.trim()),
            &(body.customer_phone.trim()),
            &(total_amount),
            &(&currency),
            &(body.delivery_address.trim()),
            &(body.delivery_detail.as_deref()),
            &(body.latitude),
            &(body.longitude),
            &(body.notes.as_deref()),
            &(body.delivery_id),
            &(body.room_number.as_deref().map(str::trim)),
            &(body.grade),
            &(body.classroom.as_deref().map(str::trim)),
        ],
    )
    .await?;

    let inserted_items = crate::db::fetch_all::<OrderItem>(
        &*tx,
        r#"
        INSERT INTO neurovector.order_items
            (order_id, item_id, quantity, unit_price, line_total, weight_grams)
        SELECT $1, requested.item_id, requested.quantity, requested.unit_price,
               requested.line_total, NULLIF(requested.weight_grams, 0)
        FROM UNNEST($2::uuid[], $3::int[], $4::int8[], $5::int8[], $6::int[])
            AS requested(item_id, quantity, unit_price, line_total, weight_grams)
        RETURNING id, order_id, item_id, quantity, unit_price, line_total, weight_grams, created_at
        "#,
        &[
            &(order.id),
            &(&item_ids),
            &(&quantities),
            &(&unit_prices),
            &(&line_totals),
            &(&weight_snapshots),
        ],
    )
    .await?;
    // INSERT RETURNING does not guarantee row order. Input item IDs are unique after validation.
    let mut inserted_by_id: HashMap<Uuid, OrderItem> = inserted_items
        .into_iter()
        .map(|item| (item.item_id, item))
        .collect();
    let items = item_ids
        .iter()
        .map(|item_id| {
            inserted_by_id.remove(item_id).ok_or_else(|| {
                AppError::Other(anyhow::anyhow!("inserted order item row is missing"))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(OrderDetail {
        order,
        items,
        payment: None,
    })
}

#[cfg(test)]
mod tests {
    use super::{validate_order_currency, validate_pin_checkout_item};
    use uuid::Uuid;

    #[test]
    fn pin_checkout_requires_one_merchant_and_positive_item_weights() {
        let merchant = Uuid::new_v4();
        assert_eq!(
            validate_pin_checkout_item(true, None, Some(merchant), Some(250)).unwrap(),
            Some(merchant)
        );
        assert!(validate_pin_checkout_item(true, None, None, Some(250)).is_err());
        assert!(validate_pin_checkout_item(true, None, Some(merchant), Some(0)).is_err());
        assert!(
            validate_pin_checkout_item(true, Some(merchant), Some(Uuid::new_v4()), Some(250))
                .is_err()
        );
        assert!(validate_pin_checkout_item(false, None, None, None).is_ok());
    }

    #[test]
    fn checkout_rejects_mixed_currency_items() {
        assert!(validate_order_currency(None, "KRW").is_ok());
        assert!(validate_order_currency(Some("KRW"), "KRW").is_ok());
        assert!(validate_order_currency(Some("KRW"), "USD").is_err());
    }
}
