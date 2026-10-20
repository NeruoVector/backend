use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    middleware,
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::{
    auth,
    cache::{Cache, CacheInfo},
    error::AppError,
    order_queue::{self, EnqueueOrderResponse, OrderEvent, OrderJob},
    routes::orders::Order,
    state::AppState,
};

pub fn router(state: AppState) -> Router<AppState> {
    let protected = Router::new()
        .route("/orders", get(admin_list_orders))
        .route("/orders/stats", get(admin_order_stats))
        .route("/order-jobs", get(admin_list_jobs))
        .route("/order-jobs/{id}/retry", post(admin_retry_job))
        .route("/order-jobs/{id}/cancel", post(admin_cancel_job))
        .route("/order-events", get(admin_list_events))
        .route("/queue/stats", get(admin_queue_stats))
        .route("/cache/stats", get(admin_cache_stats))
        .route("/auth/me", get(auth::me))
        .route_layer(middleware::from_fn_with_state(state, auth::require_admin));

    Router::new()
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        .merge(protected)
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AdminOrderListParams {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub phone: Option<String>,
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub from: Option<DateTime<Utc>>,
    #[serde(default)]
    pub to: Option<DateTime<Utc>>,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub offset: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize, Clone, ToSchema)]
pub struct AdminOrderList {
    pub items: Vec<Order>,
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone, FromRow, ToSchema)]
pub struct AdminOrderStats {
    pub total_orders: i64,
    pub pending_payment: i64,
    pub paid: i64,
    pub preparing: i64,
    pub ready: i64,
    pub assigned: i64,
    pub in_transit: i64,
    pub delivered: i64,
    pub cancelled: i64,
    pub failed: i64,
    pub today_orders: i64,
    pub revenue: i64,
    pub delivered_revenue: i64,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListJobsParams {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListEventsParams {
    #[serde(default)]
    pub order_id: Option<Uuid>,
    #[serde(default)]
    pub event_type: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize, FromRow, ToSchema)]
pub struct QueueStatusCount {
    pub status: String,
    pub count: i64,
}

#[derive(Debug, Serialize, FromRow, ToSchema)]
pub struct ConsumerOffset {
    pub consumer: String,
    pub topic: String,
    pub partition: i32,
    pub next_offset: i64,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct QueueStats {
    pub jobs: Vec<QueueStatusCount>,
    pub events: i64,
    pub consumer_offsets: Vec<ConsumerOffset>,
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/orders",
    params(AdminOrderListParams),
    responses((status = 200, description = "관리자 주문 목록", body = AdminOrderList)),
    security(("admin_session" = [])),
    tag = "admin"
)]
pub(crate) async fn admin_list_orders(
    State(state): State<AppState>,
    Query(params): Query<AdminOrderListParams>,
) -> Result<(HeaderMap, Json<AdminOrderList>), AppError> {
    let limit = params.limit.unwrap_or(50).clamp(1, 200);
    let offset = params.offset.unwrap_or(0).max(0);
    let key = format!(
        "admin:orders:list:{}:{}:{}:{}:{}:{}:{}",
        params.status.as_deref().unwrap_or("-"),
        params.phone.as_deref().unwrap_or("-"),
        params.q.as_deref().unwrap_or("-"),
        params
            .from
            .map(|value| value.to_rfc3339())
            .unwrap_or_else(|| "-".to_string()),
        params
            .to
            .map(|value| value.to_rfc3339())
            .unwrap_or_else(|| "-".to_string()),
        limit,
        offset
    );

    if let Some(cached) = state.cache.get::<AdminOrderList>(&key).await {
        return Ok((Cache::headers(true), Json(cached)));
    }

    let total = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT count(*)
        FROM neurovector.orders
        WHERE ($1::text IS NULL OR status = $1)
          AND ($2::text IS NULL OR customer_phone = $2)
          AND (
              $3::text IS NULL
              OR customer_name ILIKE '%' || $3 || '%'
              OR order_number ILIKE '%' || $3 || '%'
              OR delivery_address ILIKE '%' || $3 || '%'
          )
          AND ($4::timestamptz IS NULL OR created_at >= $4)
          AND ($5::timestamptz IS NULL OR created_at < $5)
        "#,
    )
    .bind(params.status.as_deref())
    .bind(params.phone.as_deref())
    .bind(params.q.as_deref())
    .bind(params.from)
    .bind(params.to)
    .fetch_one(&state.pool)
    .await?;

    let items = sqlx::query_as::<_, Order>(
        r#"
        SELECT id, order_number, customer_name, customer_phone, status, total_amount,
               currency, delivery_address, delivery_detail, latitude, longitude,
               notes, delivery_id, stock_restored_at, created_at, updated_at
        FROM neurovector.orders
        WHERE ($1::text IS NULL OR status = $1)
          AND ($2::text IS NULL OR customer_phone = $2)
          AND (
              $3::text IS NULL
              OR customer_name ILIKE '%' || $3 || '%'
              OR order_number ILIKE '%' || $3 || '%'
              OR delivery_address ILIKE '%' || $3 || '%'
          )
          AND ($4::timestamptz IS NULL OR created_at >= $4)
          AND ($5::timestamptz IS NULL OR created_at < $5)
        ORDER BY created_at DESC
        LIMIT $6 OFFSET $7
        "#,
    )
    .bind(params.status.as_deref())
    .bind(params.phone.as_deref())
    .bind(params.q.as_deref())
    .bind(params.from)
    .bind(params.to)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.pool)
    .await?;

    let response = AdminOrderList {
        items,
        total,
        limit,
        offset,
    };
    if let Err(err) = state.cache.set(&key, &response).await {
        tracing::warn!(error = %err, "failed to cache admin order list");
    }

    Ok((Cache::headers(false), Json(response)))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/orders/stats",
    responses((status = 200, description = "주문 통계", body = AdminOrderStats)),
    security(("admin_session" = [])),
    tag = "admin"
)]
pub(crate) async fn admin_order_stats(
    State(state): State<AppState>,
) -> Result<Json<AdminOrderStats>, AppError> {
    let key = "admin:orders:stats";
    if let Some(cached) = state.cache.get::<AdminOrderStats>(key).await {
        return Ok(Json(cached));
    }

    let stats = sqlx::query_as::<_, AdminOrderStats>(
        r#"
        SELECT count(*) AS total_orders,
               count(*) FILTER (WHERE status = 'pending_payment') AS pending_payment,
               count(*) FILTER (WHERE status = 'paid') AS paid,
               count(*) FILTER (WHERE status = 'preparing') AS preparing,
               count(*) FILTER (WHERE status = 'ready') AS ready,
               count(*) FILTER (WHERE status = 'assigned') AS assigned,
               count(*) FILTER (WHERE status = 'in_transit') AS in_transit,
               count(*) FILTER (WHERE status = 'delivered') AS delivered,
               count(*) FILTER (WHERE status = 'cancelled') AS cancelled,
               count(*) FILTER (WHERE status = 'failed') AS failed,
               count(*) FILTER (WHERE created_at >= date_trunc('day', now())) AS today_orders,
               COALESCE(sum(total_amount) FILTER (
                   WHERE status IN ('paid', 'preparing', 'ready', 'assigned', 'in_transit', 'delivered')
               ), 0)::bigint AS revenue,
               COALESCE(sum(total_amount) FILTER (WHERE status = 'delivered'), 0)::bigint AS delivered_revenue
        FROM neurovector.orders
        "#,
    )
    .fetch_one(&state.pool)
    .await?;

    if let Err(err) = state.cache.set(key, &stats).await {
        tracing::warn!(error = %err, "failed to cache admin order stats");
    }

    Ok(Json(stats))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/order-jobs",
    params(ListJobsParams),
    responses((status = 200, description = "Kafka 주문 작업 목록", body = Vec<OrderJob>)),
    security(("admin_session" = [])),
    tag = "admin"
)]
pub(crate) async fn admin_list_jobs(
    State(state): State<AppState>,
    Query(params): Query<ListJobsParams>,
) -> Result<Json<Vec<OrderJob>>, AppError> {
    let limit = params.limit.unwrap_or(50).clamp(1, 200);
    let jobs = sqlx::query_as::<_, OrderJob>(
        r#"
        SELECT id, status, payload, attempts, max_attempts, order_id, last_error,
               created_at, updated_at, started_at, completed_at
        FROM neurovector.order_jobs
        WHERE ($1::text IS NULL OR status = $1)
        ORDER BY created_at DESC
        LIMIT $2
        "#,
    )
    .bind(params.status.as_deref())
    .bind(limit)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(jobs))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/order-jobs/{id}/retry",
    params(("id" = Uuid, Path, description = "주문 작업 ID")),
    responses(
        (status = 202, description = "재발행 접수", body = EnqueueOrderResponse),
        (status = 409, description = "재시도 불가 상태")
    ),
    security(("admin_session" = [])),
    tag = "admin"
)]
pub(crate) async fn admin_retry_job(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<(StatusCode, Json<EnqueueOrderResponse>), AppError> {
    let response = order_queue::retry_job(&state, id).await?;
    Ok((StatusCode::ACCEPTED, Json(response)))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/order-jobs/{id}/cancel",
    params(("id" = Uuid, Path, description = "주문 작업 ID")),
    responses(
        (status = 200, description = "작업 취소", body = OrderJob),
        (status = 409, description = "취소 불가 상태")
    ),
    security(("admin_session" = [])),
    tag = "admin"
)]
pub(crate) async fn admin_cancel_job(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<OrderJob>, AppError> {
    Ok(Json(order_queue::cancel_job(&state, id).await?))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/order-events",
    params(ListEventsParams),
    responses((status = 200, description = "주문 이벤트 목록", body = Vec<OrderEvent>)),
    security(("admin_session" = [])),
    tag = "admin"
)]
pub(crate) async fn admin_list_events(
    State(state): State<AppState>,
    Query(params): Query<ListEventsParams>,
) -> Result<Json<Vec<OrderEvent>>, AppError> {
    let limit = params.limit.unwrap_or(100).clamp(1, 500);
    let events = sqlx::query_as::<_, OrderEvent>(
        r#"
        SELECT event_id, event_type, order_id, job_id, payload, created_at
        FROM neurovector.order_events
        WHERE ($1::uuid IS NULL OR order_id = $1)
          AND ($2::text IS NULL OR event_type = $2)
        ORDER BY created_at DESC
        LIMIT $3
        "#,
    )
    .bind(params.order_id)
    .bind(params.event_type.as_deref())
    .bind(limit)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(events))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/queue/stats",
    responses((status = 200, description = "Kafka 큐 통계", body = QueueStats)),
    security(("admin_session" = [])),
    tag = "admin"
)]
pub(crate) async fn admin_queue_stats(
    State(state): State<AppState>,
) -> Result<Json<QueueStats>, AppError> {
    let jobs = sqlx::query_as::<_, QueueStatusCount>(
        r#"
        SELECT status, count(*) AS count
        FROM neurovector.order_jobs
        GROUP BY status
        ORDER BY status
        "#,
    )
    .fetch_all(&state.pool)
    .await?;

    let events = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM neurovector.order_events")
        .fetch_one(&state.pool)
        .await?;

    let consumer_offsets = sqlx::query_as::<_, ConsumerOffset>(
        r#"
        SELECT consumer, topic, partition, next_offset, updated_at
        FROM neurovector.kafka_consumer_offsets
        ORDER BY consumer, topic, partition
        "#,
    )
    .fetch_all(&state.pool)
    .await?;

    Ok(Json(QueueStats {
        jobs,
        events,
        consumer_offsets,
    }))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/cache/stats",
    responses((status = 200, description = "Redis 캐시 통계", body = CacheInfo)),
    security(("admin_session" = [])),
    tag = "admin"
)]
pub(crate) async fn admin_cache_stats(State(state): State<AppState>) -> Json<CacheInfo> {
    Json(state.cache.info().await)
}
