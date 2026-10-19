use std::env;

use anyhow::Context;

#[derive(Debug, Clone)]
pub struct Config {
    pub listen_addr: String,
    pub public_base_url: String,
    pub database_url: String,
    pub database_max_connections: u32,
    pub redis_url: String,
    pub kafka_brokers: String,
    pub kafka_topic_events: String,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let listen_addr = env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8081".to_string());
        let public_base_url =
            env::var("PUBLIC_BASE_URL").unwrap_or_else(|_| "http://localhost:8081".to_string());

        let database_url =
            env::var("DATABASE_URL").context("DATABASE_URL must be set (see .env.example)")?;

        let database_max_connections = env::var("DATABASE_MAX_CONNECTIONS")
            .ok()
            .map(|v| v.parse::<u32>())
            .transpose()
            .context("DATABASE_MAX_CONNECTIONS must be a number")?
            .unwrap_or(10);

        let redis_url =
            env::var("REDIS_URL").context("REDIS_URL must be set (see .env.example)")?;

        let kafka_brokers =
            env::var("KAFKA_BROKERS").unwrap_or_else(|_| "localhost:9092".to_string());
        let kafka_topic_events =
            env::var("KAFKA_TOPIC_EVENTS").unwrap_or_else(|_| "neurovector.events".to_string());

        Ok(Self {
            listen_addr,
            public_base_url,
            database_url,
            database_max_connections,
            redis_url,
            kafka_brokers,
            kafka_topic_events,
        })
    }
}
