use utoipa::{
    OpenApi,
    openapi::security::{ApiKey, ApiKeyValue, SecurityScheme},
};

struct SecurityAddon;

impl utoipa::Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                "admin_session",
                SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::new(
                    crate::auth::SESSION_COOKIE_NAME,
                ))),
            );
        }
    }
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "NeuroVector API",
        version = env!("CARGO_PKG_VERSION"),
        description = "드론 배송, 비전 감지, 주문/재고/결제, 관리자 세션 로그인/큐 API"
    ),
    tags(
        (name = "orders", description = "주문 생성, 결제, Kafka 비동기 주문"),
        (name = "inventory", description = "상품 재고"),
        (name = "admin", description = "관리자 로그인, 주문 관리, Kafka 큐, Redis 캐시 통계"),
        (name = "system", description = "상태 확인"),
        (name = "routes", description = "경로 계획·창문 수령·드론 교차 검증")
    ),
    modifiers(&SecurityAddon),
    paths(
        crate::mission::create_plan,
        crate::mission::get_plan,
        crate::mission::cross_check,
        crate::root,
        crate::health,
        crate::health_ready,
        crate::auth::login,
        crate::auth::logout,
        crate::auth::me,
        crate::routes::orders::list_orders,
        crate::routes::orders::create_order,
        crate::routes::orders::enqueue_order,
        crate::routes::orders::get_order_job,
        crate::routes::orders::get_order,
        crate::routes::orders::update_order_status,
        crate::routes::orders::list_order_payments,
        crate::routes::orders::pay_order,
        crate::routes::orders::get_payment,
        crate::routes::orders::refund_payment,
        crate::routes::inventory::list_items,
        crate::routes::inventory::get_item,
        crate::routes::inventory::create_item,
        crate::routes::inventory::update_item,
        crate::routes::inventory::restock_item,
        crate::routes::admin::admin_list_orders,
        crate::routes::admin::admin_order_stats,
        crate::routes::admin::admin_list_jobs,
        crate::routes::admin::admin_retry_job,
        crate::routes::admin::admin_cancel_job,
        crate::routes::admin::admin_list_events,
        crate::routes::admin::admin_queue_stats,
        crate::routes::admin::admin_cache_stats
    ),
    components(schemas(
        crate::mission::PlanInput,
        crate::mission::MissionPlan,
        crate::mission::CrossCheck,
        crate::mission::WindowTarget,
        crate::RootResponse,
        crate::HealthResponse,
        crate::ReadinessResponse,
        crate::auth::LoginRequest,
        crate::auth::AdminSessionInfo,
        crate::routes::orders::Order,
        crate::routes::orders::OrderItem,
        crate::routes::orders::OrderDetail,
        crate::routes::orders::Payment,
        crate::routes::orders::CreateOrder,
        crate::routes::orders::CreateOrderItem,
        crate::routes::orders::UpdateOrderStatus,
        crate::routes::orders::CreatePayment,
        crate::routes::inventory::InventoryItem,
        crate::routes::inventory::CreateItem,
        crate::routes::inventory::UpdateItem,
        crate::routes::inventory::RestockItem,
        crate::order_queue::OrderJob,
        crate::order_queue::OrderEvent,
        crate::order_queue::EnqueueOrderResponse,
        crate::routes::admin::AdminOrderList,
        crate::routes::admin::AdminOrderStats,
        crate::routes::admin::QueueStats,
        crate::routes::admin::QueueStatusCount,
        crate::routes::admin::ConsumerOffset,
        crate::cache::CacheInfo
    ))
)]
pub struct ApiDoc;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openapi_contains_order_queue_and_admin_paths() {
        let value = serde_json::to_value(ApiDoc::openapi()).expect("OpenAPI serializes");
        let paths = &value["paths"];

        assert!(paths.get("/api/v1/orders/queue").is_some());
        assert!(paths.get("/api/v1/orders/jobs/{id}").is_some());
        assert!(paths.get("/api/v1/admin/order-jobs/{id}/retry").is_some());
        assert!(paths.get("/api/v1/admin/cache/stats").is_some());
        assert!(paths.get("/api/v1/admin/auth/login").is_some());
        assert!(paths.get("/api/v1/admin/auth/me").is_some());
        assert!(
            value["paths"]["/api/v1/admin/orders"]["get"]
                .get("security")
                .is_some()
        );
        assert!(
            value["components"]["securitySchemes"]
                .get("admin_session")
                .is_some()
        );
        assert!(
            value["components"]["schemas"]
                .get("EnqueueOrderResponse")
                .is_some()
        );
    }
}
