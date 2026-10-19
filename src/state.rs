use redis::Client;
use sqlx::PgPool;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub redis: Client,
    pub kafka_brokers: String,
    pub kafka_topic_events: String,
}
