use std::collections::{HashMap, HashSet};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, patch, post},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{Postgres, Transaction};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::{cache::Cache, error::AppError, order_queue, state::AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/orders", get(list_orders).post(create_order))
        .route("/orders/queue", post(enqueue_order))
        .route("/orders/jobs/{id}", get(get_order_job))
        .route("/orders/{id}", get(get_order))
        .route("/orders/{id}/status", patch(update_order_status))
        .route(
            "/orders/{id}/payments",
            get(list_order_payments).post(pay_order),
        )
        .route("/payments/{id}", get(get_payment))
        .route("/payments/{id}/refund", post(refund_payment))
}

#[derive(Debug, Serialize, Deserialize, Clone, sqlx::FromRow, ToSchema)]
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

#[derive(Debug, Serialize, Deserialize, Clone, sqlx::FromRow, ToSchema)]
pub struct OrderItem {
    pub id: Uuid,
    pub order_id: Uuid,
    pub item_id: Uuid,
    pub quantity: i32,
    pub unit_price: i64,
    pub line_total: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize, Clone, sqlx::FromRow, ToSchema)]
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

#[derive(Debug, Serialize, Deserialize, Clone, ToSchema)]
pub struct OrderDetail {
    #[serde(flatten)]
    pub order: Order,
    pub items: Vec<OrderItem>,
    pub payment: Option<Payment>,
}

#[derive(Debug, Serialize, Deserialize, Clone, ToSchema)]
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

#[derive(Debug, Serialize, Deserialize, Clone, ToSchema)]
pub struct CreateOrderItem {
    pub item_id: Uuid,
    pub quantity: i32,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListOrdersParams {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub phone: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateOrderStatus {
    pub status: String,
}

#[derive(Debug, Deserialize, ToSchema)]
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

#[utoipa::path(
    get,
    path = "/api/v1/orders",
    params(ListOrdersParams),
    responses((status = 200, description = "주문 목록", body = Vec<Order>)),
    tag = "orders"
)]
pub(crate) async fn list_orders(
    State(state): State<AppState>,
    Query(params): Query<ListOrdersParams>,
) -> Result<(HeaderMap, Json<Vec<Order>>), AppError> {
    let limit = params.limit.unwrap_or(100).clamp(1, 500);
    let key = format!(
        "orders:list:{}:{}:{}",
        params.status.as_deref().unwrap_or("-"),
        params.phone.as_deref().unwrap_or("-"),
        limit
    );

    if let Some(cached) = state.cache.get::<Vec<Order>>(&key).await {
        return Ok((Cache::headers(true), Json(cached)));
    }

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

    if let Err(err) = state.cache.set(&key, &orders).await {
        tracing::warn!(error = %err, "failed to cache order list");
    }

    Ok((Cache::headers(false), Json(orders)))
}

#[utoipa::path(
    get,
    path = "/api/v1/orders/{id}",
    params(("id" = Uuid, Path, description = "주문 ID")),
    responses(
        (status = 200, description = "주문 상세", body = OrderDetail),
        (status = 404, description = "주문 없음")
    ),
    tag = "orders"
)]
pub(crate) async fn get_order(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Json<OrderDetail>), AppError> {
    let key = format!("orders:detail:{id}");
    if let Some(cached) = state.cache.get::<OrderDetail>(&key).await {
        return Ok((Cache::headers(true), Json(cached)));
    }

    let order = fetch_order(&state, id).await?;
    let items = fetch_order_items(&state, id).await?;
    let payment = fetch_latest_payment(&state, id).await?;
    let detail = OrderDetail {
        order,
        items,
        payment,
    };

    if let Err(err) = state.cache.set(&key, &detail).await {
        tracing::warn!(error = %err, "failed to cache order detail");
    }

    Ok((Cache::headers(false), Json(detail)))
}

pub(crate) fn validate_create_order(body: &CreateOrder) -> Result<(), AppError> {
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

    Ok(())
}

/// 동기 API 와 Kafka worker 가 공유하는 주문 생성 트랜잭션.
pub(crate) async fn create_order_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    body: &CreateOrder,
) -> Result<OrderDetail, AppError> {
    if let Some(delivery_id) = body.delivery_id {
        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM neurovector.deliveries WHERE id = $1)",
        )
        .bind(delivery_id)
        .fetch_one(&mut **tx)
        .await?;
        if !exists {
            return Err(AppError::BadRequest("delivery_id does not exist".into()));
        }
    }

    let item_ids: Vec<Uuid> = body.items.iter().map(|item| item.item_id).collect();
    // Every order acquires inventory locks in the same order, regardless of input order.
    let stock_items = sqlx::query_as::<_, StockItem>(
        r#"
        SELECT id, name, unit_price, currency, stock_quantity, active
        FROM neurovector.inventory_items
        WHERE id = ANY($1::uuid[])
        ORDER BY id
        FOR UPDATE
        "#,
    )
    .bind(&item_ids)
    .fetch_all(&mut **tx)
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

        quantities.push(requested.quantity);
        unit_prices.push(item.unit_price);
        line_totals.push(line_total);
    }

    // All quantities and amounts have been checked while holding the inventory locks.
    sqlx::query(
        r#"
        UPDATE neurovector.inventory_items AS inventory
        SET stock_quantity = inventory.stock_quantity - requested.quantity, updated_at = now()
        FROM UNNEST($1::uuid[], $2::int[]) AS requested(item_id, quantity)
        WHERE inventory.id = requested.item_id
        "#,
    )
    .bind(&item_ids)
    .bind(&quantities)
    .execute(&mut **tx)
    .await?;

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
    .fetch_one(&mut **tx)
    .await?;

    let inserted_items = sqlx::query_as::<_, OrderItem>(
        r#"
        INSERT INTO neurovector.order_items
            (order_id, item_id, quantity, unit_price, line_total)
        SELECT $1, requested.item_id, requested.quantity, requested.unit_price, requested.line_total
        FROM UNNEST($2::uuid[], $3::int[], $4::int8[], $5::int8[])
            AS requested(item_id, quantity, unit_price, line_total)
        RETURNING id, order_id, item_id, quantity, unit_price, line_total, created_at
        "#,
    )
    .bind(order.id)
    .bind(&item_ids)
    .bind(&quantities)
    .bind(&unit_prices)
    .bind(&line_totals)
    .fetch_all(&mut **tx)
    .await?;
    // INSERT RETURNING does not guarantee row order. Input item IDs are unique after validation.
    let mut inserted_by_id: HashMap<Uuid, OrderItem> = inserted_items
        .into_iter()
        .map(|item| (item.item_id, item))
        .collect();
    let items = item_ids
        .iter()
        .map(|item_id| {
            inserted_by_id
                .remove(item_id)
                .ok_or(AppError::Database(sqlx::Error::RowNotFound))
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(OrderDetail {
        order,
        items,
        payment: None,
    })
}

#[utoipa::path(
    post,
    path = "/api/v1/orders",
    request_body = CreateOrder,
    responses(
        (status = 201, description = "주문 생성", body = OrderDetail),
        (status = 400, description = "잘못된 요청"),
        (status = 409, description = "재고/상태 충돌")
    ),
    tag = "orders"
)]
pub(crate) async fn create_order(
    State(state): State<AppState>,
    Json(body): Json<CreateOrder>,
) -> Result<(StatusCode, Json<OrderDetail>), AppError> {
    validate_create_order(&body)?;

    let mut tx = state.pool.begin().await?;
    let detail = create_order_in_tx(&mut tx, &body).await?;
    tx.commit().await?;

    invalidate_order_cache(&state).await;
    order_queue::publish_order_event(
        &state,
        "order.created",
        detail.order.id,
        None,
        json!({
            "order_number": &detail.order.order_number,
            "status": &detail.order.status,
            "total_amount": detail.order.total_amount,
            "currency": &detail.order.currency,
            "source": "sync_api",
        }),
    )
    .await;

    Ok((StatusCode::CREATED, Json(detail)))
}

/// Kafka 로 주문 생성을 비동기 처리한다. 응답은 202 이며 `/orders/jobs/{id}` 로
/// 작업 상태를 조회할 수 있다.
#[utoipa::path(
    post,
    path = "/api/v1/orders/queue",
    request_body = CreateOrder,
    responses(
        (status = 202, description = "Kafka 주문 작업 접수", body = order_queue::EnqueueOrderResponse),
        (status = 400, description = "잘못된 요청")
    ),
    tag = "orders"
)]
pub(crate) async fn enqueue_order(
    State(state): State<AppState>,
    Json(body): Json<CreateOrder>,
) -> Result<(StatusCode, Json<order_queue::EnqueueOrderResponse>), AppError> {
    let response = order_queue::enqueue_create_order(&state, body).await?;
    Ok((StatusCode::ACCEPTED, Json(response)))
}

#[utoipa::path(
    get,
    path = "/api/v1/orders/jobs/{id}",
    params(("id" = Uuid, Path, description = "주문 작업 ID")),
    responses(
        (status = 200, description = "주문 작업 상태", body = order_queue::OrderJob),
        (status = 404, description = "작업 없음")
    ),
    tag = "orders"
)]
pub(crate) async fn get_order_job(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<order_queue::OrderJob>, AppError> {
    Ok(Json(order_queue::get_job(&state.pool, id).await?))
}

pub(crate) async fn invalidate_order_cache(state: &AppState) {
    // Order creation/cancellation/refunds also change inventory. Scan once for
    // all affected namespaces rather than traversing the Redis keyspace three times.
    if let Err(err) = state
        .cache
        .delete_prefixes(&["orders:", "admin:orders:", "inventory:"])
        .await
    {
        tracing::warn!(error = %err, "failed to invalidate order and inventory caches");
    }
}

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
        ORDER BY item_id
        "#,
    )
    .bind(order_id)
    .fetch_all(&mut **tx)
    .await?;

    // Match order creation's inventory lock order during cancellation and refund.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_order() -> CreateOrder {
        CreateOrder {
            customer_name: "Test User".into(),
            customer_phone: "010-0000-0000".into(),
            items: vec![CreateOrderItem {
                item_id: Uuid::new_v4(),
                quantity: 1,
            }],
            delivery_address: "Seoul".into(),
            delivery_detail: None,
            latitude: Some(37.5),
            longitude: Some(127.0),
            notes: None,
            delivery_id: None,
        }
    }

    #[test]
    fn accepts_valid_order() {
        assert!(validate_create_order(&valid_order()).is_ok());
    }

    #[test]
    fn rejects_duplicate_items_and_bad_coordinates() {
        let mut order = valid_order();
        order.items.push(order.items[0].clone());
        assert!(validate_create_order(&order).is_err());

        let mut order = valid_order();
        order.latitude = Some(120.0);
        assert!(validate_create_order(&order).is_err());

        let mut order = valid_order();
        order.latitude = Some(37.5);
        order.longitude = None;
        assert!(validate_create_order(&order).is_err());
    }
}
