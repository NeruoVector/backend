use sqlx::PgPool;

use crate::{cache::Cache, events::KafkaPublisher, media::MediaClient};

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub redis: crate::redis_store::RedisStore,
    pub cache: Cache,
    pub kafka: KafkaPublisher,
    /// 주문 비동기 큐 토픽 전용 publisher.
    pub order_queue: KafkaPublisher,
    pub media: MediaClient,
    pub admin_session_ttl_seconds: u64,
    pub admin_cookie_secure: bool,
    pub max_altitude_m: f64,
    pub route_max_leg_m: f64,
    pub route_max_distance_m: f64,
    pub route_battery_wh: f64,
    pub route_power_w: f64,
    pub risk_lookback_days: i64,
    pub risk_corridor_m: f64,
    pub telemetry_online_ttl_seconds: u64,
}

#[cfg(test)]
pub(crate) fn test_state(pool: PgPool) -> AppState {
    let redis = crate::redis_store::RedisStore::new(
        redis::Client::open("redis://127.0.0.1:6399/0").expect("redis url"),
        std::time::Duration::from_millis(500),
    );
    AppState {
        pool,
        redis: redis.clone(),
        cache: Cache::new(redis, false, 30),
        kafka: KafkaPublisher::new("127.0.0.1:19092", "neurovector.events"),
        order_queue: KafkaPublisher::new("127.0.0.1:19092", "neurovector.orders"),
        media: MediaClient::new(
            "http://127.0.0.1:9997",
            "http://127.0.0.1:8889",
            "localhost",
            8890,
            std::path::PathBuf::from("/tmp"),
        ),
        admin_session_ttl_seconds: 86_400,
        admin_cookie_secure: false,
        max_altitude_m: 120.0,
        route_max_leg_m: 5000.0,
        route_max_distance_m: 20_000.0,
        route_battery_wh: 500.0,
        route_power_w: 300.0,
        risk_lookback_days: 90,
        risk_corridor_m: 150.0,
        telemetry_online_ttl_seconds: 120,
    }
}
