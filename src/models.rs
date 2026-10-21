use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 장애물 감지 1건 (DB row + API 응답).
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Detection {
    pub id: Uuid,
    pub drone_id: String,
    pub label: String,
    pub confidence: f32,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub distance_m: Option<f64>,
    pub detected_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

/// HTTP / MQTT 로 들어오는 감지 이벤트.
#[derive(Debug, Deserialize)]
pub struct CreateDetection {
    pub drone_id: String,
    pub label: String,
    pub confidence: f32,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub distance_m: Option<f64>,
    pub detected_at: Option<DateTime<Utc>>,
}

/// 비행 세션 (DB row + API 응답).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct FlightSession {
    pub id: Uuid,
    pub drone_id: String,
    pub delivery_id: Option<Uuid>,
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// 텔레메트리 1건 (DB row + API 응답).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Telemetry {
    pub id: i64,
    pub drone_id: String,
    pub flight_id: Option<Uuid>,
    pub ts: DateTime<Utc>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub altitude_m: Option<f64>,
    pub speed_mps: Option<f64>,
    pub vertical_speed_mps: Option<f64>,
    pub battery_pct: Option<f64>,
    pub heading_deg: Option<f64>,
    pub status: Option<String>,
    pub received_at: DateTime<Utc>,
}

/// MQTT `neurovector/drones/{drone_id}/telemetry` 페이로드.
#[derive(Debug, Serialize, Deserialize)]
pub struct TelemetryMessage {
    pub ts: Option<DateTime<Utc>>,
    #[serde(alias = "lat")]
    pub latitude: Option<f64>,
    #[serde(alias = "lon", alias = "lng")]
    pub longitude: Option<f64>,
    #[serde(alias = "alt", alias = "altitude")]
    pub altitude_m: Option<f64>,
    #[serde(alias = "ground_speed", alias = "speed")]
    pub speed_mps: Option<f64>,
    #[serde(alias = "vz", alias = "climb_rate")]
    pub vertical_speed_mps: Option<f64>,
    #[serde(alias = "battery", alias = "battery_percent")]
    pub battery_pct: Option<f64>,
    #[serde(alias = "heading", alias = "yaw")]
    pub heading_deg: Option<f64>,
    pub status: Option<String>,
    pub flight_id: Option<Uuid>,
}
