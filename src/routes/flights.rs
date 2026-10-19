use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    error::AppError,
    geo, ingest,
    models::{FlightSession, Telemetry, TelemetryMessage},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_flights))
        .route("/status", get(list_status))
        .route("/status/{drone_id}", get(get_status))
        .route("/telemetry/{drone_id}", post(post_telemetry))
        .route("/{id}", get(get_flight))
        .route("/{id}/telemetry", get(list_flight_telemetry))
        .route("/{id}/analysis", get(analyze_flight))
        .route("/{id}/end", post(end_flight))
}

#[derive(Debug, Serialize)]
pub struct DroneStatus {
    pub drone_id: String,
    pub online: bool,
    pub last_seen: DateTime<Utc>,
    pub stale_for_seconds: i64,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub altitude_m: Option<f64>,
    pub speed_mps: Option<f64>,
    pub battery_pct: Option<f64>,
    pub heading_deg: Option<f64>,
    pub status: Option<String>,
    pub flight_id: Option<Uuid>,
    pub flight_status: Option<String>,
    pub delivery_id: Option<Uuid>,
}

#[derive(Debug, sqlx::FromRow)]
struct StatusRow {
    drone_id: String,
    last_seen: DateTime<Utc>,
    latitude: Option<f64>,
    longitude: Option<f64>,
    altitude_m: Option<f64>,
    speed_mps: Option<f64>,
    battery_pct: Option<f64>,
    heading_deg: Option<f64>,
    status: Option<String>,
    flight_id: Option<Uuid>,
    flight_status: Option<String>,
    delivery_id: Option<Uuid>,
}

impl StatusRow {
    fn into_status(self, ttl_seconds: u64) -> DroneStatus {
        let stale_for_seconds = (Utc::now() - self.last_seen).num_seconds().max(0);
        DroneStatus {
            drone_id: self.drone_id,
            online: stale_for_seconds <= ttl_seconds as i64,
            last_seen: self.last_seen,
            stale_for_seconds,
            latitude: self.latitude,
            longitude: self.longitude,
            altitude_m: self.altitude_m,
            speed_mps: self.speed_mps,
            battery_pct: self.battery_pct,
            heading_deg: self.heading_deg,
            status: self.status,
            flight_id: self.flight_id,
            flight_status: self.flight_status,
            delivery_id: self.delivery_id,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ListFlightsParams {
    #[serde(default)]
    pub drone_id: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct ListTelemetryParams {
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct EndFlight {
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct FlightAnalysis {
    pub flight: FlightSession,
    pub telemetry_count: i64,
    pub duration_seconds: f64,
    pub distance_m: f64,
    pub average_speed_mps: Option<f64>,
    pub max_speed_mps: Option<f64>,
    pub max_altitude_m: Option<f64>,
    pub min_altitude_m: Option<f64>,
    pub battery_start_pct: Option<f64>,
    pub battery_end_pct: Option<f64>,
    pub battery_drop_pct: Option<f64>,
    pub battery_drop_per_minute: Option<f64>,
    pub telemetry_gap_count: i64,
    pub max_altitude_violations: i64,
    pub anomalies: Vec<FlightAnomaly>,
    pub summary: String,
}

#[derive(Debug, Serialize)]
pub struct FlightAnomaly {
    pub kind: &'static str,
    pub severity: &'static str,
    pub message: String,
    pub at: Option<DateTime<Utc>>,
}

async fn list_status(State(state): State<AppState>) -> Result<Json<Vec<DroneStatus>>, AppError> {
    let rows = sqlx::query_as::<_, StatusRow>(
        r#"
        SELECT DISTINCT ON (t.drone_id)
               t.drone_id, t.ts AS last_seen, t.latitude, t.longitude, t.altitude_m,
               t.speed_mps, t.battery_pct, t.heading_deg, t.status,
               fs.id AS flight_id, fs.status AS flight_status, fs.delivery_id
        FROM neurovector.telemetry t
        LEFT JOIN neurovector.flight_sessions fs
               ON fs.drone_id = t.drone_id AND fs.ended_at IS NULL
        ORDER BY t.drone_id, t.ts DESC
        "#,
    )
    .fetch_all(&state.pool)
    .await?;

    let statuses = rows
        .into_iter()
        .map(|row| row.into_status(state.telemetry_online_ttl_seconds))
        .collect();
    Ok(Json(statuses))
}

async fn get_status(
    State(state): State<AppState>,
    Path(drone_id): Path<String>,
) -> Result<Json<DroneStatus>, AppError> {
    let row = sqlx::query_as::<_, StatusRow>(
        r#"
        SELECT t.drone_id, t.ts AS last_seen, t.latitude, t.longitude, t.altitude_m,
               t.speed_mps, t.battery_pct, t.heading_deg, t.status,
               fs.id AS flight_id, fs.status AS flight_status, fs.delivery_id
        FROM neurovector.telemetry t
        LEFT JOIN neurovector.flight_sessions fs
               ON fs.drone_id = t.drone_id AND fs.ended_at IS NULL
        WHERE t.drone_id = $1
        ORDER BY t.ts DESC
        LIMIT 1
        "#,
    )
    .bind(&drone_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::NotFound)?;

    Ok(Json(row.into_status(state.telemetry_online_ttl_seconds)))
}

async fn list_flights(
    State(state): State<AppState>,
    Query(params): Query<ListFlightsParams>,
) -> Result<Json<Vec<FlightSession>>, AppError> {
    let limit = params.limit.unwrap_or(50).clamp(1, 200);
    let flights = sqlx::query_as::<_, FlightSession>(
        r#"
        SELECT id, drone_id, delivery_id, status, started_at, ended_at, created_at
        FROM neurovector.flight_sessions
        WHERE ($1::text IS NULL OR drone_id = $1)
          AND ($2::text IS NULL OR status = $2)
        ORDER BY started_at DESC
        LIMIT $3
        "#,
    )
    .bind(params.drone_id.as_deref())
    .bind(params.status.as_deref())
    .bind(limit)
    .fetch_all(&state.pool)
    .await?;

    Ok(Json(flights))
}

async fn get_flight(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<FlightSession>, AppError> {
    let flight = sqlx::query_as::<_, FlightSession>(
        r#"
        SELECT id, drone_id, delivery_id, status, started_at, ended_at, created_at
        FROM neurovector.flight_sessions
        WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::NotFound)?;

    Ok(Json(flight))
}

async fn list_flight_telemetry(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(params): Query<ListTelemetryParams>,
) -> Result<Json<Vec<Telemetry>>, AppError> {
    let limit = params.limit.unwrap_or(500).clamp(1, 5_000);
    let rows = sqlx::query_as::<_, Telemetry>(
        r#"
        SELECT id, drone_id, flight_id, ts, latitude, longitude, altitude_m, speed_mps,
               vertical_speed_mps, battery_pct, heading_deg, status, received_at
        FROM neurovector.telemetry
        WHERE flight_id = $1
        ORDER BY ts ASC
        LIMIT $2
        "#,
    )
    .bind(id)
    .bind(limit)
    .fetch_all(&state.pool)
    .await?;

    Ok(Json(rows))
}

async fn post_telemetry(
    State(state): State<AppState>,
    Path(drone_id): Path<String>,
    Json(body): Json<TelemetryMessage>,
) -> Result<(axum::http::StatusCode, Json<Telemetry>), AppError> {
    let telemetry = ingest::ingest_telemetry(&state, drone_id, body).await?;
    Ok((axum::http::StatusCode::CREATED, Json(telemetry)))
}

async fn end_flight(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<EndFlight>,
) -> Result<Json<FlightSession>, AppError> {
    let status = body.status.as_deref().unwrap_or("completed");
    if !matches!(status, "completed" | "failed" | "aborted") {
        return Err(AppError::BadRequest(
            "status must be completed, failed, or aborted".into(),
        ));
    }

    let flight = sqlx::query_as::<_, FlightSession>(
        r#"
        UPDATE neurovector.flight_sessions
        SET status = $2, ended_at = COALESCE(ended_at, now())
        WHERE id = $1
        RETURNING id, drone_id, delivery_id, status, started_at, ended_at, created_at
        "#,
    )
    .bind(id)
    .bind(status)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::NotFound)?;

    Ok(Json(flight))
}

async fn analyze_flight(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<FlightAnalysis>, AppError> {
    let flight = sqlx::query_as::<_, FlightSession>(
        r#"
        SELECT id, drone_id, delivery_id, status, started_at, ended_at, created_at
        FROM neurovector.flight_sessions
        WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::NotFound)?;

    let telemetry = sqlx::query_as::<_, Telemetry>(
        r#"
        SELECT id, drone_id, flight_id, ts, latitude, longitude, altitude_m, speed_mps,
               vertical_speed_mps, battery_pct, heading_deg, status, received_at
        FROM neurovector.telemetry
        WHERE flight_id = $1
        ORDER BY ts ASC
        "#,
    )
    .bind(id)
    .fetch_all(&state.pool)
    .await?;

    let mut anomalies = Vec::new();
    let mut distance_m = 0.0;
    let mut max_speed: Option<f64> = None;
    let mut max_altitude: Option<f64> = None;
    let mut min_altitude: Option<f64> = None;
    let mut speed_sum = 0.0;
    let mut speed_count = 0usize;
    let mut gap_count = 0i64;
    let mut altitude_violations = 0i64;

    for (index, row) in telemetry.iter().enumerate() {
        if let (Some(lat), Some(lon)) = (row.latitude, row.longitude)
            && let Some(previous) = index.checked_sub(1).and_then(|i| telemetry.get(i))
            && let (Some(prev_lat), Some(prev_lon)) = (previous.latitude, previous.longitude)
        {
            distance_m += geo::haversine_m((prev_lat, prev_lon), (lat, lon));
        }

        if let Some(speed) = row.speed_mps {
            max_speed = Some(max_speed.map_or(speed, |current| current.max(speed)));
            speed_sum += speed;
            speed_count += 1;
            if speed > 30.0 {
                anomalies.push(FlightAnomaly {
                    kind: "overspeed",
                    severity: "warning",
                    message: format!("speed {speed:.1}m/s exceeds 30m/s"),
                    at: Some(row.ts),
                });
            }
        }
        if let Some(altitude) = row.altitude_m {
            max_altitude = Some(max_altitude.map_or(altitude, |current| current.max(altitude)));
            min_altitude = Some(min_altitude.map_or(altitude, |current| current.min(altitude)));
            if altitude > state.max_altitude_m {
                altitude_violations += 1;
            }
        }
        if let Some(battery) = row.battery_pct
            && battery < 20.0
        {
            anomalies.push(FlightAnomaly {
                kind: "low_battery",
                severity: if battery < 10.0 {
                    "critical"
                } else {
                    "warning"
                },
                message: format!("battery at {battery:.1}%"),
                at: Some(row.ts),
            });
        }
        if let Some(previous) = index.checked_sub(1).and_then(|i| telemetry.get(i)) {
            let gap = (row.ts - previous.ts).num_seconds();
            if gap > 30 {
                gap_count += 1;
                anomalies.push(FlightAnomaly {
                    kind: "telemetry_gap",
                    severity: "info",
                    message: format!("no telemetry for {gap}s"),
                    at: Some(row.ts),
                });
            }
        }
    }

    if altitude_violations > 0 {
        anomalies.push(FlightAnomaly {
            kind: "altitude_limit",
            severity: "critical",
            message: format!(
                "{altitude_violations} telemetry point(s) above {}m",
                state.max_altitude_m
            ),
            at: None,
        });
    }

    let first = telemetry.first();
    let last = telemetry.last();
    let battery_start = first.and_then(|row| row.battery_pct);
    let battery_end = last.and_then(|row| row.battery_pct);
    let battery_drop = match (battery_start, battery_end) {
        (Some(start), Some(end)) => Some((start - end).max(0.0)),
        _ => None,
    };

    let duration_seconds = match (first, last) {
        (Some(first), Some(last)) if last.ts > first.ts => {
            (last.ts - first.ts).num_seconds() as f64
        }
        _ => flight
            .ended_at
            .map(|ended| (ended - flight.started_at).num_seconds() as f64)
            .unwrap_or(0.0)
            .max(0.0),
    };
    let battery_drop_per_minute = battery_drop.and_then(|drop| {
        if duration_seconds > 0.0 {
            Some(drop / (duration_seconds / 60.0))
        } else {
            None
        }
    });

    let summary = if telemetry.is_empty() {
        "No telemetry was recorded for this flight.".to_string()
    } else if anomalies.is_empty() {
        format!(
            "Flight completed normally over {:.1} km with {} telemetry points.",
            distance_m / 1000.0,
            telemetry.len()
        )
    } else {
        format!(
            "Flight covered {:.1} km with {} anomaly signal(s).",
            distance_m / 1000.0,
            anomalies.len()
        )
    };

    Ok(Json(FlightAnalysis {
        flight,
        telemetry_count: telemetry.len() as i64,
        duration_seconds,
        distance_m,
        average_speed_mps: (speed_count > 0).then_some(speed_sum / speed_count as f64),
        max_speed_mps: max_speed,
        max_altitude_m: max_altitude,
        min_altitude_m: min_altitude,
        battery_start_pct: battery_start,
        battery_end_pct: battery_end,
        battery_drop_pct: battery_drop,
        battery_drop_per_minute,
        telemetry_gap_count: gap_count,
        max_altitude_violations: altitude_violations,
        anomalies,
        summary,
    }))
}
