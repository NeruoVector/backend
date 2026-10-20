use redis::Client;
use sqlx::PgPool;

use crate::{cache::Cache, events::KafkaPublisher, media::MediaClient};

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub redis: Client,
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
