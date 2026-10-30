//! Customer-owned delivery progress and telemetry-backed arrival events.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue},
    routing::get,
};
use chrono::{Duration, Utc};
use uuid::Uuid;

use crate::{customer_auth, error::AppError, state::AppState};

pub(crate) mod arrival;
mod arrival_rules;
mod eta;
mod types;

pub use arrival::record_arrival_events;
use types::{EventRow, TrackingRow};
pub use types::{OrderTracking, TrackingDestination, TrackingDrone, TrackingEvent};

const TELEMETRY_FRESH_SECONDS: i64 = arrival_rules::TELEMETRY_FRESH_SECONDS;

pub fn router() -> Router<AppState> {
    Router::new().route("/account/orders/{id}/tracking", get(get_order_tracking))
}

async fn get_order_tracking(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(order_id): Path<Uuid>,
) -> Result<(HeaderMap, Json<OrderTracking>), AppError> {
    let customer = customer_auth::current(&state, &headers).await?;
    let row = crate::db::fetch_optional::<TrackingRow>(
        &state.pool.get().await?,
        r#"
        SELECT o.id AS order_id, o.status AS order_status,
               d.id AS delivery_id, d.status AS delivery_status,
               d.scheduled_at, d.drone_id,
               f.cruise_speed_mps, f.capacity_grams,
               o.latitude, o.longitude, o.room_number, o.grade, o.classroom,
               s.stop_sequence, s.eta_at AS stop_eta_at,
               s.estimated_weight_grams::bigint AS estimated_weight_grams, s.arrived_at,
               telemetry.ts AS telemetry_ts,
               telemetry.received_at AS telemetry_received_at,
               telemetry.latitude AS telemetry_latitude,
               telemetry.longitude AS telemetry_longitude,
               telemetry.speed_mps AS telemetry_speed_mps,
               weight.weight_grams AS measured_payload_weight_grams,
               weight.measured_at
        FROM neurovector.orders o
        LEFT JOIN neurovector.deliveries d ON d.id = o.delivery_id
        LEFT JOIN neurovector.delivery_fleet f ON f.drone_id = d.drone_id
        LEFT JOIN LATERAL (
            SELECT current_stop.id, current_stop.stop_sequence, current_stop.eta_at,
                   current_stop.estimated_weight_grams, current_stop.arrived_at
            FROM neurovector.delivery_stops current_stop
            WHERE current_stop.order_id = o.id
            ORDER BY current_stop.created_at DESC, current_stop.id DESC
            LIMIT 1
        ) s ON true
        LEFT JOIN LATERAL (
            SELECT t.ts, t.received_at, t.latitude, t.longitude, t.speed_mps
            FROM neurovector.current_drone_telemetry t
            JOIN neurovector.flight_sessions fs
              ON fs.id = t.flight_id AND fs.delivery_id = d.id
            WHERE t.drone_id = d.drone_id
            ORDER BY t.received_at DESC, t.ts DESC, t.id DESC
            LIMIT 1
        ) telemetry ON true
        LEFT JOIN LATERAL (
            SELECT m.weight_grams, m.measured_at
            FROM neurovector.delivery_weight_measurements m
            WHERE m.delivery_id = d.id
            ORDER BY m.measured_at DESC, m.id DESC
            LIMIT 1
        ) weight ON true
        WHERE o.id = $1 AND o.customer_id = $2
        "#,
        &[&(order_id), &(customer.id)],
    )
    .await?
    .ok_or(AppError::NotFound)?;

    let now = Utc::now();
    let telemetry_fresh = arrival_rules::telemetry_is_fresh(
        row.telemetry_ts,
        row.telemetry_received_at,
        now,
        Duration::seconds(TELEMETRY_FRESH_SECONDS),
    );
    let drone = row.drone_id.as_ref().map(|drone_id| TrackingDrone {
        drone_id: drone_id.clone(),
        latitude: row.telemetry_latitude,
        longitude: row.telemetry_longitude,
        speed_mps: row.telemetry_speed_mps,
        observed_at: row.telemetry_ts,
        stale: !telemetry_fresh,
    });

    let (eta_at, eta_basis) = if row.order_status == "delivered" || row.arrived_at.is_some() {
        (None, "none")
    } else if telemetry_fresh
        && matches!(
            row.delivery_status.as_deref(),
            Some("dispatched" | "in_flight")
        )
        && row.telemetry_latitude.is_some()
        && row.telemetry_longitude.is_some()
        && row.latitude.is_some()
        && row.longitude.is_some()
        && row.delivery_id.is_some()
        && row.stop_sequence.is_some()
    {
        let delivery_id = row.delivery_id.ok_or(AppError::NotFound)?;
        let sequence = row.stop_sequence.ok_or(AppError::NotFound)?;
        let eta = eta::telemetry_eta(&state.pool, &row, delivery_id, sequence, now).await?;
        match eta {
            Some(value) => (Some(value), "telemetry"),
            None => (row.stop_eta_at, "scheduled_route"),
        }
    } else if row.stop_eta_at.is_some() {
        (row.stop_eta_at, "scheduled_route")
    } else {
        (None, "none")
    };

    let events = crate::db::fetch_all::<EventRow>(
        &state.pool.get().await?,
        r#"
        SELECT id, event_type, occurred_at, payload
        FROM neurovector.order_tracking_events
        WHERE order_id = $1
        ORDER BY occurred_at, id
        "#,
        &[&(order_id)],
    )
    .await?
    .into_iter()
    .map(TrackingEvent::from)
    .collect::<Vec<_>>();
    let arrival_notification_id = events
        .iter()
        .find(|event| event.event_type == "arrived")
        .map(|event| event.id);
    let payload_over_capacity = row
        .measured_payload_weight_grams
        .zip(row.capacity_grams)
        .map(|(measured, capacity)| measured > capacity);

    let tracking = OrderTracking {
        order_id: row.order_id,
        order_status: row.order_status,
        delivery_status: row.delivery_status,
        scheduled_at: row.scheduled_at,
        eta_at,
        eta_basis,
        drone,
        destination: TrackingDestination {
            latitude: row.latitude,
            longitude: row.longitude,
            room_number: row.room_number,
            grade: row.grade,
            classroom: row.classroom,
        },
        estimated_weight_grams: row.estimated_weight_grams,
        measured_payload_weight_grams: row.measured_payload_weight_grams,
        measured_at: row.measured_at,
        payload_over_capacity,
        arrived_at: row.arrived_at,
        arrival_notification_id,
        events,
    };
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    Ok((response_headers, Json(tracking)))
}
