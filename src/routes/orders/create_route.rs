use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use serde_json::json;
use uuid::Uuid;

use crate::{error::AppError, order_queue, state::AppState};

use super::{
    creation::create_order_in_tx,
    invalidate_order_cache,
    models::{CreateOrder, OrderDetail},
    validation::validate_create_order,
};

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
    headers: HeaderMap,
    Json(body): Json<CreateOrder>,
) -> Result<(StatusCode, Json<OrderDetail>), AppError> {
    validate_create_order(&body)?;
    let customer = if body.handoff_pin.is_some() || body.spot_id.is_some() {
        Some(crate::customer_auth::current(&state, &headers).await?)
    } else {
        None
    };
    let handoff = match (customer.as_ref(), body.handoff_pin.as_deref()) {
        (Some(customer), Some(pin)) => {
            Some(crate::handoff::prepare_order_handoff(customer.id, pin).await?)
        }
        (None, Some(_)) => return Err(AppError::Unauthorized("로그인이 필요합니다.".into())),
        (_, None) => None,
    };

    let mut connection = state.pool.get().await?;
    let mut tx = connection.transaction().await?;
    let mut detail = create_order_in_tx(&mut tx, &body).await?;
    if let Some(customer) = customer.as_ref() {
        detail.order = crate::customer_order::attach(
            &mut tx,
            detail.order.id,
            crate::customer_order::CustomerOrderContext {
                customer_id: customer.id,
                spot_id: body.spot_id,
            },
        )
        .await?;
    }
    if let Some(handoff) = &handoff {
        crate::handoff::store_order_handoff(&mut tx, detail.order.id, handoff).await?;
    }
    tx.commit().await?;
    drop(connection);

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
    if body.handoff_pin.is_some() || body.spot_id.is_some() {
        return Err(AppError::BadRequest(
            "customer handoff and saved spot checkout requires the synchronous orders endpoint"
                .into(),
        ));
    }
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
