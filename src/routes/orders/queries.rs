use axum::{
    Json,
    extract::{Path, Query, State},
    http::HeaderMap,
};
use uuid::Uuid;

use crate::{cache::Cache, error::AppError, state::AppState};

use super::{
    access,
    models::{ListOrdersParams, Order, OrderDetail},
    persistence::{fetch_latest_payment, fetch_order, fetch_order_items},
};

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
    headers: HeaderMap,
) -> Result<(HeaderMap, Json<Vec<Order>>), AppError> {
    let actor = access::actor(&state.pool, &headers).await?;
    if actor == access::Actor::Anonymous {
        return Err(AppError::Unauthorized("sign in to access orders".into()));
    }
    let limit = params.limit.unwrap_or(100).clamp(1, 500);
    let key = format!(
        "orders:list:{}:{}:{}:{}",
        match actor {
            access::Actor::Admin => "admin".to_string(),
            access::Actor::Customer(id) => format!("customer:{id}"),
            access::Actor::Anonymous => unreachable!(),
        },
        params.status.as_deref().unwrap_or("-"),
        params.phone.as_deref().unwrap_or("-"),
        limit
    );

    if let Some(cached) = state.cache.get::<Vec<Order>>(&key).await {
        return Ok((Cache::headers(true), Json(cached)));
    }

    let orders = crate::db::fetch_all::<Order>(
        &state.pool.get().await?,
        r#"
        SELECT id, order_number, customer_name, customer_phone, status, total_amount,
               currency, delivery_address, delivery_detail, latitude, longitude,
               notes, delivery_id, stock_restored_at, created_at, updated_at
        FROM neurovector.orders
        WHERE ($1::text IS NULL OR status = $1)
          AND ($2::text IS NULL OR customer_phone = $2)
          AND ($4::uuid IS NULL OR customer_id = $4)
        ORDER BY created_at DESC
        LIMIT $3
        "#,
        &[
            &(params.status.as_deref()),
            &(params.phone.as_deref()),
            &(limit),
            &(match actor {
                access::Actor::Customer(id) => Some(id),
                access::Actor::Admin => None,
                access::Actor::Anonymous => unreachable!(),
            }),
        ],
    )
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
    headers: HeaderMap,
) -> Result<(HeaderMap, Json<OrderDetail>), AppError> {
    let access = access::authorize_order(&state.pool, &headers, id).await?;
    let key = format!("orders:detail:{}:{id}", access.cache_scope());
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
