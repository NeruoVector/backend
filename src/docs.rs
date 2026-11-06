use utoipa::{
    OpenApi,
    openapi::security::{ApiKey, ApiKeyValue, SecurityScheme},
};

struct SecurityAddon;

impl utoipa::Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        if let Some(components) = openapi.components.as_mut() {
            components.add_security_scheme(
                "user_session",
                SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::new(
                    crate::routes::users::COOKIE_NAME,
                ))),
            );
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
        description = "드론 배송, 비전 감지, 주문/재고/결제, 관리자 세션 로그인/큐, 리뷰, SRT/WebRTC WHEP, 학교 시뮬레이션 API"
    ),
    tags(
        (name = "users", description = "일반 사용자 등록 및 세션"),
        (name = "schools", description = "등록 학교 코드 조회"),
        (name = "notifications", description = "서비스 내 알림 발송 및 조회"),
        (name = "placeholders", description = "형식만 정의한 미구현 API"),
        (name = "orders", description = "주문 생성, 결제, Kafka 비동기 주문"),
        (name = "reviews", description = "상품 리뷰 및 결제 주문 리뷰·태그·사진 저장과 조회"),
        (name = "streams", description = "SRT 송출 정보, WebRTC WHEP 세션, 녹화"),
        (name = "simulation", description = "학교 시뮬레이션 주문 피드"),
        (name = "inventory", description = "상품 재고"),
        (name = "admin", description = "관리자 로그인, 주문 관리, Kafka 큐, Redis 캐시 통계"),
        (name = "system", description = "상태 확인"),
        (name = "routes", description = "경로 계획·창문 수령·드론 교차 검증")
    ),
    modifiers(&SecurityAddon),
    paths(
        crate::routes::users::register,
        crate::routes::users::login,
        crate::routes::users::me,
        crate::routes::users::logout,
        crate::routes::customer::list_schools,
        crate::routes::customer::get_school,
        crate::routes::customer::save_school,
        crate::routes::customer::list_review_feed,
        crate::routes::customer::create_review,
        crate::routes::customer::update_review,
        crate::routes::customer::delete_review,
        crate::routes::customer::send_notification,
        crate::routes::customer::list_notifications,
        crate::routes::customer::read_notification,
        crate::routes::customer::request_phone,
        crate::routes::customer::confirm_phone,
        crate::routes::customer::save_payment_method,
        crate::cards::list,
        crate::cards::create,
        crate::cards::remove,
        crate::customer_auth::register,
        crate::customer_auth::login,
        crate::customer_auth::me,
        crate::customer_auth::logout,
        crate::customer_auth::orders,
        crate::customer_auth::order,
        crate::customer_auth::reviews,
        crate::routes::reviews::create_review,
        crate::routes::reviews::get_review,
        crate::routes::reviews::photo,
        crate::routes::streams::list_streams,
        crate::routes::streams::stream_info,
        crate::routes::streams::whep_offer,
        crate::routes::streams::whep_delete,
        crate::routes::streams::list_recordings,
        crate::routes::streams::download_recording,
        crate::routes::simulation::orders,
        crate::routes::receipts::receive,
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
        crate::cards::DemoKind,
        crate::cards::CardInput,
        crate::cards::Card,
        crate::cards::CardList,
        crate::routes::reviews::CreateReview,
        crate::routes::reviews::Review,
        crate::routes::reviews::ReviewList,
        crate::routes::streams::StreamInfo,
        crate::media::Recording,
        crate::routes::simulation::DispatchOrder,
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
    fn openapi_preserves_customer_and_order_review_contracts_after_merge() {
        // Given: the complete merged API document.
        let value = serde_json::to_value(ApiDoc::openapi()).unwrap();
        // When: consumers discover both review contracts and authentication flows.
        let paths = &value["paths"];
        let schemas = &value["components"]["schemas"];
        // Then: the shared GET documents dispatch and distinct response schemas.
        for path in [
            "/api/v1/users",
            "/api/v1/users/session",
            "/api/v1/schools",
            "/api/v1/notifications",
            "/api/v1/auth/register",
            "/api/v1/auth/cards",
            "/api/v1/orders/{id}/receipt",
            "/api/v1/orders/{id}/review",
        ] {
            assert!(paths.get(path).is_some(), "missing {path}");
        }
        assert!(paths["/api/v1/reviews"].get("post").is_some());
        assert!(paths["/api/v1/reviews/{id}"].get("put").is_some());
        assert!(paths["/api/v1/reviews/{id}"].get("delete").is_some());
        let params = paths["/api/v1/reviews"]["get"]["parameters"]
            .as_array()
            .unwrap();
        assert!(
            params
                .iter()
                .any(|param| param["name"] == "item_id" && param["required"] == false)
        );
        assert!(params.iter().any(|param| param["name"] == "phone"));
        assert!(schemas["ItemReview"]["properties"].get("item_id").is_some());
        assert!(schemas["Review"]["properties"].get("order_id").is_some());
        assert_eq!(schemas["ReviewFeed"]["oneOf"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn openapi_contains_reviews_and_webrtc_contracts() {
        let value = serde_json::to_value(ApiDoc::openapi()).expect("OpenAPI serializes");
        for path in [
            "/api/v1/orders/{id}/review",
            "/api/v1/reviews",
            "/api/v1/reviews/{id}/photo",
            "/api/v1/streams",
            "/api/v1/streams/{drone_id}/whep",
            "/api/v1/streams/{drone_id}/whep/{session_id}",
            "/api/v1/recordings",
            "/api/v1/simulation/orders",
        ] {
            assert!(value["paths"].get(path).is_some(), "missing {path}");
        }
        let offer = &value["paths"]["/api/v1/streams/{drone_id}/whep"]["post"];
        assert!(
            offer["requestBody"]["content"]
                .get("application/sdp")
                .is_some()
        );
        assert!(
            offer["responses"]["201"]["headers"]
                .get("Location")
                .is_some()
        );
        assert!(value["components"]["schemas"].get("CreateReview").is_some());
        let photo_content = value["paths"]["/api/v1/reviews/{id}/photo"]["get"]["responses"]["200"]
            ["content"]
            .as_object()
            .expect("photo response has content types");
        assert_eq!(photo_content.len(), 3);
        for mime in ["image/jpeg", "image/png", "image/webp"] {
            assert!(
                photo_content.contains_key(mime),
                "missing photo type {mime}"
            );
        }
    }

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
