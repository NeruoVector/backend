use std::collections::{HashMap, HashSet};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, patch, post},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::{error::AppError, state::AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/orders", get(list_orders).post(create_order))
        .route("/orders/{id}", get(get_order))
        .route("/orders/{id}/status", patch(update_order_status))
        .route(
            "/orders/{id}/payments",
            get(list_order_payments).post(pay_order),
        )
        .route("/payments/{id}", get(get_payment))
        .route("/payments/{id}/refund", post(refund_payment))
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Order {
    pub id: Uuid,
    pub order_number: String,
    pub customer_name: String,
    pub customer_phone: String,
    pub status: String,
    pub total_amount: i64,
    pub currency: String,
    pub delivery_address: String,
    pub delivery_detail: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub notes: Option<String>,
    pub delivery_id: Option<Uuid>,
    pub stock_restored_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct OrderItem {
    pub id: Uuid,
    pub order_id: Uuid,
    pub item_id: Uuid,
    pub quantity: i32,
    pub unit_price: i64,
    pub line_total: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Payment {
    pub id: Uuid,
    pub order_id: Uuid,
    pub provider: String,
    pub method: String,
    pub status: String,
    pub amount: i64,
    pub currency: String,
    pub transaction_id: Option<String>,
    pub failure_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct OrderDetail {
    #[serde(flatten)]
    pub order: Order,
    pub items: Vec<OrderItem>,
    pub payment: Option<Payment>,
}

#[derive(Debug, Deserialize)]
pub struct CreateOrder {
    pub customer_name: String,
    pub customer_phone: String,
    pub items: Vec<CreateOrderItem>,
    pub delivery_address: String,
    #[serde(default)]
    pub delivery_detail: Option<String>,
    #[serde(default)]
    pub latitude: Option<f64>,
    #[serde(default)]
    pub longitude: Option<f64>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub delivery_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct CreateOrderItem {
    pub item_id: Uuid,
    pub quantity: i32,
}

#[derive(Debug, Deserialize)]
pub struct ListOrdersParams {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub phone: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateOrderStatus {
    pub status: String,
}

#[derive(Debug, Deserialize)]
pub struct CreatePayment {
    #[serde(default)]
    pub method: Option<String>,
    /// `success` 또는 `failure`. 더미 결제 시뮬레이션에 사용합니다.
    #[serde(default)]
    pub simulate: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct StockItem {
    id: Uuid,
    name: String,
    unit_price: i64,
    currency: String,
    stock_quantity: i32,
    active: bool,
}

async fn list_orders(
    State(state): State<AppState>,
    Query(params): Query<ListOrdersParams>,
) -> Result<Json<Vec<Order>>, AppError> {
    let limit = params.limit.unwrap_or(100).clamp(1, 500);
    let orders = sqlx::query_as::<_, Order>(
        r#"
        SELECT id, order_number, customer_name, customer_phone, status, total_amount,
               currency, delivery_address, delivery_detail, latitude, longitude,
               notes, delivery_id, stock_restored_at, created_at, updated_at
        FROM neurovector.orders
        WHERE ($1::text IS NULL OR status = $1)
          AND ($2::text IS NULL OR customer_phone = $2)
        ORDER BY created_at DESC
        LIMIT $3
        "#,
    )
    .bind(params.status.as_deref())
    .bind(params.phone.as_deref())
    .bind(limit)
    .fetch_all(&state.pool)
    .await?;

    Ok(Json(orders))
}

async fn get_order(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<OrderDetail>, AppError> {
    let order = fetch_order(&state, id).await?;
    let items = fetch_order_items(&state, id).await?;
    let payment = fetch_latest_payment(&state, id).await?;
    Ok(Json(OrderDetail {
        order,
        items,
        payment,
    }))
}

async fn create_order(
    State(state): State<AppState>,
    Json(body): Json<CreateOrder>,
) -> Result<(StatusCode, Json<OrderDetail>), AppError> {
    if body.customer_name.trim().is_empty() {
        return Err(AppError::BadRequest("customer_name is required".into()));
    }
    if body.customer_phone.trim().is_empty() {
        return Err(AppError::BadRequest("customer_phone is required".into()));
    }
    if body.delivery_address.trim().is_empty() {
        return Err(AppError::BadRequest("delivery_address is required".into()));
    }
    if body.items.is_empty() {
        return Err(AppError::BadRequest(
            "at least one order item is required".into(),
        ));
    }
    if body.latitude.is_some() != body.longitude.is_some() {
        return Err(AppError::BadRequest(
            "latitude and longitude must be provided together".into(),
        ));
    }
    if let Some(lat) = body.latitude
        && !(-90.0..=90.0).contains(&lat)
    {
        return Err(AppError::BadRequest(
            "latitude must be between -90 and 90".into(),
        ));
    }
    if let Some(lon) = body.longitude
        && !(-180.0..=180.0).contains(&lon)
    {
        return Err(AppError::BadRequest(
            "longitude must be between -180 and 180".into(),
        ));
    }

    let mut seen = HashSet::new();
    for item in &body.items {
        if item.quantity <= 0 || item.quantity > 100 {
            return Err(AppError::BadRequest(
                "item quantity must be between 1 and 100".into(),
            ));
        }
        if !seen.insert(item.item_id) {
            return Err(AppError::BadRequest(
                "duplicate item_id in order items".into(),
            ));
        }
    }

    let mut tx = state.pool.begin().await?;

    if let Some(delivery_id) = body.delivery_id {
        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM neurovector.deliveries WHERE id = $1)",
        )
        .bind(delivery_id)
        .fetch_one(&mut *tx)
        .await?;
        if !exists {
            return Err(AppError::BadRequest("delivery_id does not exist".into()));
        }
    }

    let mut total_amount = 0i64;
    let mut currency: Option<String> = None;
    let mut line_items = Vec::with_capacity(body.items.len());

    for requested in &body.items {
        let item = sqlx::query_as::<_, StockItem>(
            r#"
            SELECT id, name, unit_price, currency, stock_quantity, active
            FROM neurovector.inventory_items
            WHERE id = $1
            FOR UPDATE
            "#,
        )
        .bind(requested.item_id)
        .fetch_optional(&mut *tx)
        .await?
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
        if let Some(currency) = &currency
            && currency != &item.currency
        {
            return Err(AppError::BadRequest(
                "all order items must use the same currency".into(),
            ));
        }
        currency = Some(item.currency.clone());

        let line_total = item
            .unit_price
            .checked_mul(requested.quantity as i64)
            .ok_or_else(|| AppError::BadRequest("order amount is too large".into()))?;
        total_amount = total_amount
            .checked_add(line_total)
            .ok_or_else(|| AppError::BadRequest("order amount is too large".into()))?;

        sqlx::query(
            r#"
            UPDATE neurovector.inventory_items
            SET stock_quantity = stock_quantity - $2, updated_at = now()
            WHERE id = $1
            "#,
        )
        .bind(item.id)
        .bind(requested.quantity)
        .execute(&mut *tx)
        .await?;

        line_items.push((item.id, requested.quantity, item.unit_price, line_total));
    }

    let currency = currency.unwrap_or_else(|| "KRW".to_string());
    let order_number = format!(
        "NV-{}-{}",
        Utc::now().format("%Y%m%d"),
        Uuid::new_v4().simple().to_string()[..8].to_uppercase()
    );

    let order = sqlx::query_as::<_, Order>(
        r#"
        INSERT INTO neurovector.orders
            (order_number, customer_name, customer_phone, status, total_amount, currency,
             delivery_address, delivery_detail, latitude, longitude, notes, delivery_id)
        VALUES ($1, $2, $3, 'pending_payment', $4, $5, $6, $7, $8, $9, $10, $11)
        RETURNING id, order_number, customer_name, customer_phone, status, total_amount,
                  currency, delivery_address, delivery_detail, latitude, longitude,
                  notes, delivery_id, stock_restored_at, created_at, updated_at
        "#,
    )
    .bind(&order_number)
    .bind(body.customer_name.trim())
    .bind(body.customer_phone.trim())
    .bind(total_amount)
    .bind(&currency)
    .bind(body.delivery_address.trim())
    .bind(body.delivery_detail.as_deref())
    .bind(body.latitude)
    .bind(body.longitude)
    .bind(body.notes.as_deref())
    .bind(body.delivery_id)
    .fetch_one(&mut *tx)
    .await?;

    let mut items = Vec::with_capacity(line_items.len());
    for (item_id, quantity, unit_price, line_total) in line_items {
        let item = sqlx::query_as::<_, OrderItem>(
            r#"
            INSERT INTO neurovector.order_items
                (order_id, item_id, quantity, unit_price, line_total)
            VALUES ($1, $2, $3, $4, $5)
            RETURNING id, order_id, item_id, quantity, unit_price, line_total, created_at
            "#,
        )
        .bind(order.id)
        .bind(item_id)
        .bind(quantity)
        .bind(unit_price)
        .bind(line_total)
        .fetch_one(&mut *tx)
        .await?;
        items.push(item);
    }

    tx.commit().await?;

    Ok((
        StatusCode::CREATED,
        Json(OrderDetail {
            order,
            items,
            payment: None,
        }),
    ))
}

async fn update_order_status(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateOrderStatus>,
) -> Result<Json<Order>, AppError> {
    let mut tx = state.pool.begin().await?;
    let current = fetch_order_for_update(&mut tx, id).await?;
    validate_order_transition(&current.status, &body.status)?;

    let stock_restored = body.status == "cancelled" && current.stock_restored_at.is_none();
    if stock_restored {
        restore_stock(&mut tx, id).await?;
    }

    let order = sqlx::query_as::<_, Order>(
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
    )
    .bind(id)
    .bind(&body.status)
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(Json(order))
}

async fn list_order_payments(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<Payment>>, AppError> {
    let payments = sqlx::query_as::<_, Payment>(
        r#"
        SELECT id, order_id, provider, method, status, amount, currency,
               transaction_id, failure_reason, created_at, updated_at
        FROM neurovector.payments
        WHERE order_id = $1
        ORDER BY created_at DESC
        "#,
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(payments))
}

async fn pay_order(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<CreatePayment>,
) -> Result<(StatusCode, Json<Payment>), AppError> {
    let method = body.method.unwrap_or_else(|| "dummy_card".to_string());
    let simulate = body.simulate.unwrap_or_else(|| "success".to_string());
    if !matches!(simulate.as_str(), "success" | "failure") {
        return Err(AppError::BadRequest(
            "simulate must be 'success' or 'failure'".into(),
        ));
    }

    let mut tx = state.pool.begin().await?;
    let order = fetch_order_for_update(&mut tx, id).await?;

    if order.status != "pending_payment" {
        return Err(AppError::Conflict(format!(
            "order is not awaiting payment (status: {})",
            order.status
        )));
    }
    let already_captured = sqlx::query_scalar::<_, bool>(
        r#"
        SELECT EXISTS(
            SELECT 1 FROM neurovector.payments
            WHERE order_id = $1 AND status IN ('authorized', 'captured')
        )
        "#,
    )
    .bind(id)
    .fetch_one(&mut *tx)
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

    let payment = sqlx::query_as::<_, Payment>(
        r#"
        INSERT INTO neurovector.payments
            (order_id, provider, method, status, amount, currency, transaction_id, failure_reason)
        VALUES ($1, 'dummy', $2, $3, $4, $5, $6, $7)
        RETURNING id, order_id, provider, method, status, amount, currency,
                  transaction_id, failure_reason, created_at, updated_at
        "#,
    )
    .bind(order.id)
    .bind(&method)
    .bind(status)
    .bind(order.total_amount)
    .bind(&order.currency)
    .bind(transaction_id)
    .bind(failure_reason)
    .fetch_one(&mut *tx)
    .await?;

    if status == "captured" {
        sqlx::query(
            "UPDATE neurovector.orders SET status = 'paid', updated_at = now() WHERE id = $1",
        )
        .bind(order.id)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;

    let code = if status == "captured" {
        StatusCode::CREATED
    } else {
        StatusCode::PAYMENT_REQUIRED
    };
    Ok((code, Json(payment)))
}

async fn get_payment(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Payment>, AppError> {
    let payment = sqlx::query_as::<_, Payment>(
        r#"
        SELECT id, order_id, provider, method, status, amount, currency,
               transaction_id, failure_reason, created_at, updated_at
        FROM neurovector.payments
        WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::NotFound)?;
    Ok(Json(payment))
}

async fn refund_payment(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Payment>, AppError> {
    let mut tx = state.pool.begin().await?;
    let payment = sqlx::query_as::<_, Payment>(
        r#"
        SELECT id, order_id, provider, method, status, amount, currency,
               transaction_id, failure_reason, created_at, updated_at
        FROM neurovector.payments
        WHERE id = $1
        FOR UPDATE
        "#,
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;

    if payment.status != "captured" {
        return Err(AppError::Conflict(
            "only captured payments can be refunded".into(),
        ));
    }

    let order = fetch_order_for_update(&mut tx, payment.order_id).await?;
    if order.stock_restored_at.is_none() && order.status != "delivered" {
        restore_stock(&mut tx, order.id).await?;
    }

    let payment = sqlx::query_as::<_, Payment>(
        r#"
        UPDATE neurovector.payments
        SET status = 'refunded', updated_at = now()
        WHERE id = $1
        RETURNING id, order_id, provider, method, status, amount, currency,
                  transaction_id, failure_reason, created_at, updated_at
        "#,
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;

    if order.status != "delivered" && order.status != "cancelled" {
        sqlx::query(
            r#"
            UPDATE neurovector.orders
            SET status = 'cancelled',
                stock_restored_at = COALESCE(stock_restored_at, now()),
                updated_at = now()
            WHERE id = $1
            "#,
        )
        .bind(order.id)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(Json(payment))
}

async fn fetch_order(state: &AppState, id: Uuid) -> Result<Order, AppError> {
    sqlx::query_as::<_, Order>(
        r#"
        SELECT id, order_number, customer_name, customer_phone, status, total_amount,
               currency, delivery_address, delivery_detail, latitude, longitude,
               notes, delivery_id, stock_restored_at, created_at, updated_at
        FROM neurovector.orders
        WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::NotFound)
}

async fn fetch_order_for_update(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<Order, AppError> {
    sqlx::query_as::<_, Order>(
        r#"
        SELECT id, order_number, customer_name, customer_phone, status, total_amount,
               currency, delivery_address, delivery_detail, latitude, longitude,
               notes, delivery_id, stock_restored_at, created_at, updated_at
        FROM neurovector.orders
        WHERE id = $1
        FOR UPDATE
        "#,
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(AppError::NotFound)
}

async fn fetch_order_items(state: &AppState, order_id: Uuid) -> Result<Vec<OrderItem>, AppError> {
    let items = sqlx::query_as::<_, OrderItem>(
        r#"
        SELECT id, order_id, item_id, quantity, unit_price, line_total, created_at
        FROM neurovector.order_items
        WHERE order_id = $1
        ORDER BY created_at ASC
        "#,
    )
    .bind(order_id)
    .fetch_all(&state.pool)
    .await?;
    Ok(items)
}

async fn fetch_latest_payment(
    state: &AppState,
    order_id: Uuid,
) -> Result<Option<Payment>, AppError> {
    let payment = sqlx::query_as::<_, Payment>(
        r#"
        SELECT id, order_id, provider, method, status, amount, currency,
               transaction_id, failure_reason, created_at, updated_at
        FROM neurovector.payments
        WHERE order_id = $1
        ORDER BY created_at DESC
        LIMIT 1
        "#,
    )
    .bind(order_id)
    .fetch_optional(&state.pool)
    .await?;
    Ok(payment)
}

async fn restore_stock(tx: &mut Transaction<'_, Postgres>, order_id: Uuid) -> Result<(), AppError> {
    let items = sqlx::query_as::<_, OrderItem>(
        r#"
        SELECT id, order_id, item_id, quantity, unit_price, line_total, created_at
        FROM neurovector.order_items
        WHERE order_id = $1
        "#,
    )
    .bind(order_id)
    .fetch_all(&mut **tx)
    .await?;

    for item in items {
        sqlx::query(
            "UPDATE neurovector.inventory_items SET stock_quantity = stock_quantity + $2, updated_at = now() WHERE id = $1",
        )
        .bind(item.item_id)
        .bind(item.quantity)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

fn validate_order_transition(current: &str, next: &str) -> Result<(), AppError> {
    let allowed: HashMap<&str, &[&str]> = HashMap::from([
        ("pending_payment", &["paid", "cancelled"][..]),
        ("paid", &["preparing", "cancelled"][..]),
        ("preparing", &["ready", "cancelled"][..]),
        ("ready", &["assigned", "cancelled"][..]),
        ("assigned", &["in_transit", "cancelled"][..]),
        ("in_transit", &["delivered", "failed"][..]),
        ("delivered", &[][..]),
        ("failed", &[][..]),
        ("cancelled", &[][..]),
    ]);

    if current == next {
        return Err(AppError::Conflict(format!(
            "order is already in status {current}"
        )));
    }
    let Some(allowed_next) = allowed.get(current) else {
        return Err(AppError::BadRequest(format!(
            "unknown current status {current}"
        )));
    };
    if !allowed_next.contains(&next) {
        return Err(AppError::Conflict(format!(
            "cannot transition order from {current} to {next}"
        )));
    }
    Ok(())
}
