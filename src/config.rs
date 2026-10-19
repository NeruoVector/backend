use std::{env, path::PathBuf};

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

    // MQTT (Mosquitto)
    pub mqtt_host: String,
    pub mqtt_port: u16,
    pub mqtt_username: String,
    pub mqtt_password: String,
    pub mqtt_client_id: String,
    pub mqtt_topic_prefix: String,
    pub mqtt_qos: u8,

    // MediaMTX (SRT ingest + WebRTC/WHEP playback)
    pub media_api_base_url: String,
    pub media_whep_base_url: String,
    pub media_srt_public_host: String,
    pub media_srt_port: u16,
    pub media_recordings_dir: PathBuf,

    // 경로 검증 / 위험 예측 기본값
    pub max_altitude_m: f64,
    pub route_max_leg_m: f64,
    pub route_max_distance_m: f64,
    pub route_battery_wh: f64,
    pub route_power_w: f64,
    pub risk_lookback_days: i64,
    pub risk_corridor_m: f64,
    pub telemetry_online_ttl_seconds: u64,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let listen_addr = env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8081".to_string());
        let public_base_url =
            env::var("PUBLIC_BASE_URL").unwrap_or_else(|_| "http://localhost:8081".to_string());

        let database_url =
            env::var("DATABASE_URL").context("DATABASE_URL must be set (see .env.example)")?;

        let database_max_connections = parse_env("DATABASE_MAX_CONNECTIONS", 10u32)?;
        let redis_url =
            env::var("REDIS_URL").context("REDIS_URL must be set (see .env.example)")?;

        let kafka_brokers =
            env::var("KAFKA_BROKERS").unwrap_or_else(|_| "localhost:9092".to_string());
        let kafka_topic_events =
            env::var("KAFKA_TOPIC_EVENTS").unwrap_or_else(|_| "neurovector.events".to_string());

        let mqtt_host = env::var("MQTT_HOST").unwrap_or_else(|_| "localhost".to_string());
        let mqtt_port = parse_env("MQTT_PORT", 1883u16)?;
        let mqtt_username = env::var("MQTT_USERNAME").unwrap_or_else(|_| "neurovector".to_string());
        let mqtt_password = env::var("MQTT_PASSWORD").unwrap_or_else(|_| "neurovector".to_string());
        let mqtt_client_id =
            env::var("MQTT_CLIENT_ID").unwrap_or_else(|_| "neurovector-backend".to_string());
        let mqtt_topic_prefix =
            env::var("MQTT_TOPIC_PREFIX").unwrap_or_else(|_| "neurovector".to_string());
        let mqtt_qos = parse_env("MQTT_QOS", 1u8)?;

        let media_api_base_url =
            env::var("MEDIA_API_BASE_URL").unwrap_or_else(|_| "http://localhost:9997".to_string());
        let media_whep_base_url =
            env::var("MEDIA_WHEP_BASE_URL").unwrap_or_else(|_| "http://localhost:8889".to_string());
        let media_srt_public_host =
            env::var("MEDIA_SRT_PUBLIC_HOST").unwrap_or_else(|_| "localhost".to_string());
        let media_srt_port = parse_env("MEDIA_SRT_PORT", 8890u16)?;
        let media_recordings_dir = env::var("MEDIA_RECORDINGS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("./recordings"));

        let max_altitude_m = parse_env("MAX_ALTITUDE_M", 120.0f64)?;
        let route_max_leg_m = parse_env("ROUTE_MAX_LEG_M", 5_000.0f64)?;
        let route_max_distance_m = parse_env("ROUTE_MAX_DISTANCE_M", 20_000.0f64)?;
        let route_battery_wh = parse_env("ROUTE_BATTERY_WH", 500.0f64)?;
        let route_power_w = parse_env("ROUTE_POWER_W", 300.0f64)?;
        let risk_lookback_days = parse_env("RISK_LOOKBACK_DAYS", 90i64)?;
        let risk_corridor_m = parse_env("RISK_CORRIDOR_M", 150.0f64)?;
        let telemetry_online_ttl_seconds = parse_env("TELEMETRY_ONLINE_TTL_SECONDS", 120u64)?;

        Ok(Self {
            listen_addr,
            public_base_url,
            database_url,
            database_max_connections,
            redis_url,
            kafka_brokers,
            kafka_topic_events,
            mqtt_host,
            mqtt_port,
            mqtt_username,
            mqtt_password,
            mqtt_client_id,
            mqtt_topic_prefix,
            mqtt_qos,
            media_api_base_url,
            media_whep_base_url,
            media_srt_public_host,
            media_srt_port,
            media_recordings_dir,
            max_altitude_m,
            route_max_leg_m,
            route_max_distance_m,
            route_battery_wh,
            route_power_w,
            risk_lookback_days,
            risk_corridor_m,
            telemetry_online_ttl_seconds,
        })
    }
}

fn parse_env<T>(key: &str, default: T) -> anyhow::Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match env::var(key) {
        Ok(raw) => raw
            .parse::<T>()
            .map_err(|err| anyhow::anyhow!("{key} must be valid: {err}")),
        Err(_) => Ok(default),
    }
}
