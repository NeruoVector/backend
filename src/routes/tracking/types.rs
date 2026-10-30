use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

use uuid::Uuid;

#[derive(Debug, Serialize)]
pub struct TrackingEvent {
    pub id: Uuid,
    pub event_type: String,
    pub occurred_at: DateTime<Utc>,
    pub payload: Value,
}

#[derive(Debug, Serialize)]
pub struct TrackingDrone {
    pub drone_id: String,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub speed_mps: Option<f64>,
    pub observed_at: Option<DateTime<Utc>>,
    pub stale: bool,
}

#[derive(Debug, Serialize)]
pub struct TrackingDestination {
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub room_number: Option<String>,
    pub grade: Option<i32>,
    pub classroom: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct OrderTracking {
    pub order_id: Uuid,
    pub order_status: String,
    pub delivery_status: Option<String>,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub eta_at: Option<DateTime<Utc>>,
    pub eta_basis: &'static str,
    pub drone: Option<TrackingDrone>,
    pub destination: TrackingDestination,
    pub estimated_weight_grams: Option<i64>,
    pub measured_payload_weight_grams: Option<i32>,
    pub measured_at: Option<DateTime<Utc>>,
    pub payload_over_capacity: Option<bool>,
    pub arrived_at: Option<DateTime<Utc>>,
    pub arrival_notification_id: Option<Uuid>,
    pub events: Vec<TrackingEvent>,
}

#[derive(Debug)]
pub(super) struct TrackingRow {
    pub(super) order_id: Uuid,
    pub(super) order_status: String,
    pub(super) delivery_id: Option<Uuid>,
    pub(super) delivery_status: Option<String>,
    pub(super) scheduled_at: Option<DateTime<Utc>>,
    pub(super) drone_id: Option<String>,
    pub(super) cruise_speed_mps: Option<f64>,
    pub(super) capacity_grams: Option<i32>,
    pub(super) latitude: Option<f64>,
    pub(super) longitude: Option<f64>,
    pub(super) room_number: Option<String>,
    pub(super) grade: Option<i32>,
    pub(super) classroom: Option<String>,
    pub(super) stop_sequence: Option<i32>,
    pub(super) stop_eta_at: Option<DateTime<Utc>>,
    pub(super) estimated_weight_grams: Option<i64>,
    pub(super) arrived_at: Option<DateTime<Utc>>,
    pub(super) telemetry_ts: Option<DateTime<Utc>>,
    pub(super) telemetry_received_at: Option<DateTime<Utc>>,
    pub(super) telemetry_latitude: Option<f64>,
    pub(super) telemetry_longitude: Option<f64>,
    pub(super) telemetry_speed_mps: Option<f64>,
    pub(super) measured_payload_weight_grams: Option<i32>,
    pub(super) measured_at: Option<DateTime<Utc>>,
}
crate::db::row!(TrackingRow {
    order_id,
    order_status,
    delivery_id,
    delivery_status,
    scheduled_at,
    drone_id,
    cruise_speed_mps,
    capacity_grams,
    latitude,
    longitude,
    room_number,
    grade,
    classroom,
    stop_sequence,
    stop_eta_at,
    estimated_weight_grams,
    arrived_at,
    telemetry_ts,
    telemetry_received_at,
    telemetry_latitude,
    telemetry_longitude,
    telemetry_speed_mps,
    measured_payload_weight_grams,
    measured_at
});

#[derive(Debug)]
pub(super) struct EventRow {
    pub(super) id: Uuid,
    pub(super) event_type: String,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) payload: Value,
}
crate::db::row!(EventRow {
    id,
    event_type,
    occurred_at,
    payload
});

impl From<EventRow> for TrackingEvent {
    fn from(row: EventRow) -> Self {
        Self {
            id: row.id,
            event_type: row.event_type,
            occurred_at: row.occurred_at,
            payload: row.payload,
        }
    }
}
