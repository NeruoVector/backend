use crate::state::AppState;
use axum::{
    Router,
    http::{HeaderValue, header},
    middleware::Next,
    response::Response,
    routing::{get, patch, post},
};

mod access;
pub(crate) mod create_route;
pub(crate) mod creation;
pub(crate) mod models;
pub(crate) mod payment_mutations;
pub(crate) mod payment_reads;
pub(crate) mod persistence;
pub(crate) mod queries;
pub(crate) mod status;
pub(crate) mod validation;

pub(crate) use create_route::{create_order, enqueue_order, get_order_job};
pub(crate) use creation::create_order_in_tx;
pub(crate) use models::{
    CreateOrder, CreateOrderItem, CreatePayment, Order, OrderDetail, OrderItem, Payment,
    UpdateOrderStatus,
};
pub(crate) use payment_mutations::{pay_order, refund_payment};
pub(crate) use payment_reads::{get_payment, list_order_payments};
pub(crate) use persistence::map_handoff_guard;
pub(crate) use queries::{get_order, list_orders};
pub(crate) use status::update_order_status;
pub(crate) use validation::validate_create_order;

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
        .route_layer(axum::middleware::from_fn(no_store))
        .route_layer(axum::middleware::from_fn(
            crate::customer_auth::require_same_origin,
        ))
}

async fn no_store(request: axum::extract::Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response
}

pub(crate) async fn invalidate_order_cache(state: &AppState) {
    if let Err(err) = state
        .cache
        .delete_prefixes(&["orders:", "admin:orders:", "inventory:"])
        .await
    {
        tracing::warn!(error = %err, "failed to invalidate order and inventory caches");
    }
}
