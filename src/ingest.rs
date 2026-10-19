use chrono::Utc;
use redis::AsyncCommands;
use serde_json::json;
use sqlx::PgPool;

use crate::{
    error::AppError,
    events::DetectionEvent,
    models::{CreateDetection, Detection, FlightSession, Telemetry, TelemetryMessage},
    state::AppState,
};

pub async fn ingest_detection(
    state: &AppState,
    body: CreateDetection,
) -> Result<Detection, AppError> {
    if body.drone_id.trim().is_empty() {
        return Err(AppError::BadRequest("drone_id is required".into()));
    }
    if body.label.trim().is_empty() {
        return Err(AppError::BadRequest("label is required".into()));
    }
    if !(0.0..=1.0).contains(&body.confidence) {
        return Err(AppError::BadRequest(
            "confidence must be between 0 and 1".into(),
        ));
    }
    if let Some(lat) = body.latitude
        && !(-90.0..=90.0).contains(&lat)
    {
        return Err(AppError::BadRequest(
            "latitude must be between -90 and 90".into(),
        ));
    }
    if let Some(lon) = body.longitude
        && !(-180.0..=180.0).contains(&lon)
    {
        return Err(AppError::BadRequest(
            "longitude must be between -180 and 180".into(),
        ));
    }

    let detection = sqlx::query_as::<_, Detection>(
        r#"
        INSERT INTO neurovector.detections
            (drone_id, label, confidence, latitude, longitude, detected_at)
        VALUES ($1, $2, $3, $4, $5, COALESCE($6, now()))
        RETURNING id, drone_id, label, confidence, latitude, longitude, detected_at, created_at
        "#,
    )
    .bind(&body.drone_id)
    .bind(&body.label)
    .bind(body.confidence)
    .bind(body.latitude)
    .bind(body.longitude)
    .bind(body.detected_at)
    .fetch_one(&state.pool)
    .await?;

    let event = DetectionEvent {
        id: detection.id,
        drone_id: &detection.drone_id,
        label: &detection.label,
        confidence: detection.confidence,
        detected_at: detection.detected_at,
    };

    if let Err(err) = state.kafka.publish_detection(&event).await {
        // Kafka 장애가 감지 저장 자체를 막지는 않는다.
        tracing::warn!(error = %err, detection_id = %detection.id, "failed to publish detection event");
    }

    Ok(detection)
}

pub async fn ingest_telemetry(
    state: &AppState,
    drone_id: String,
    body: TelemetryMessage,
) -> Result<Telemetry, AppError> {
    let drone_id = drone_id.trim().to_string();
    if drone_id.is_empty() {
        return Err(AppError::BadRequest("drone_id is required".into()));
    }
    validate_telemetry(&body)?;

    let status = body
        .status
        .as_deref()
        .unwrap_or("unknown")
        .to_ascii_lowercase();
    let terminal = matches!(
        status.as_str(),
        "landed" | "completed" | "idle" | "disarmed" | "aborted" | "failed"
    );
    let starting = matches!(
        status.as_str(),
        "in_flight" | "flying" | "armed" | "taking_off" | "mission" | "dispatched"
    );

    let active = find_active_flight(&state.pool, &drone_id).await?;

    let flight = match (active, terminal, starting) {
        (Some(flight), true, _) => {
            let close_status = match status.as_str() {
                "failed" => "failed",
                "aborted" => "aborted",
                _ => "completed",
            };
            let closed = sqlx::query_as::<_, FlightSession>(
                r#"
                UPDATE neurovector.flight_sessions
                SET ended_at = now(), status = $2
                WHERE id = $1
                RETURNING id, drone_id, delivery_id, status, started_at, ended_at, created_at
                "#,
            )
            .bind(flight.id)
            .bind(close_status)
            .fetch_one(&state.pool)
            .await?;
            tracing::info!(drone_id = %drone_id, flight_id = %closed.id, "flight session closed");
            Some(closed)
        }
        (Some(flight), false, _) => Some(flight),
        (None, false, true) => {
            let delivery_id = sqlx::query_scalar::<_, uuid::Uuid>(
                r#"
                SELECT id FROM neurovector.deliveries
                WHERE drone_id = $1 AND status IN ('dispatched', 'in_flight')
                ORDER BY scheduled_at DESC
                LIMIT 1
                "#,
            )
            .bind(&drone_id)
            .fetch_optional(&state.pool)
            .await?;

            let created = sqlx::query_as::<_, FlightSession>(
                r#"
                INSERT INTO neurovector.flight_sessions (drone_id, delivery_id, status)
                VALUES ($1, $2, 'in_flight')
                RETURNING id, drone_id, delivery_id, status, started_at, ended_at, created_at
                "#,
            )
            .bind(&drone_id)
            .bind(delivery_id)
            .fetch_one(&state.pool)
            .await?;
            tracing::info!(drone_id = %drone_id, flight_id = %created.id, "flight session started");
            Some(created)
        }
        (None, _, _) => None,
    };

    let raw = serde_json::to_value(&body).unwrap_or_else(|_| json!({}));
    let telemetry = sqlx::query_as::<_, Telemetry>(
        r#"
        INSERT INTO neurovector.telemetry
            (drone_id, flight_id, ts, latitude, longitude, altitude_m, speed_mps,
             vertical_speed_mps, battery_pct, heading_deg, status, raw)
        VALUES ($1, $2, COALESCE($3, now()), $4, $5, $6, $7, $8, $9, $10, $11, $12)
        RETURNING id, drone_id, flight_id, ts, latitude, longitude, altitude_m, speed_mps,
                  vertical_speed_mps, battery_pct, heading_deg, status, received_at
        "#,
    )
    .bind(&drone_id)
    .bind(flight.as_ref().map(|f| f.id))
    .bind(body.ts)
    .bind(body.latitude)
    .bind(body.longitude)
    .bind(body.altitude_m)
    .bind(body.speed_mps)
    .bind(body.vertical_speed_mps)
    .bind(body.battery_pct)
    .bind(body.heading_deg)
    .bind(body.status.as_deref())
    .bind(raw)
    .fetch_one(&state.pool)
    .await?;

    if let Err(err) = cache_latest_status(state, &telemetry).await {
        tracing::warn!(error = %err, drone_id = %drone_id, "failed to update latest status cache");
    }

    Ok(telemetry)
}

fn validate_telemetry(body: &TelemetryMessage) -> Result<(), AppError> {
    if let Some(lat) = body.latitude
        && !(-90.0..=90.0).contains(&lat)
    {
        return Err(AppError::BadRequest(
            "latitude must be between -90 and 90".into(),
        ));
    }
    if let Some(lon) = body.longitude
        && !(-180.0..=180.0).contains(&lon)
    {
        return Err(AppError::BadRequest(
            "longitude must be between -180 and 180".into(),
        ));
    }
    if let Some(battery) = body.battery_pct
        && !(0.0..=100.0).contains(&battery)
    {
        return Err(AppError::BadRequest(
            "battery_pct must be between 0 and 100".into(),
        ));
    }
    if let Some(speed) = body.speed_mps
        && speed < 0.0
    {
        return Err(AppError::BadRequest("speed_mps must be >= 0".into()));
    }
    Ok(())
}

async fn find_active_flight(
    pool: &PgPool,
    drone_id: &str,
) -> Result<Option<FlightSession>, AppError> {
    let flight = sqlx::query_as::<_, FlightSession>(
        r#"
        SELECT id, drone_id, delivery_id, status, started_at, ended_at, created_at
        FROM neurovector.flight_sessions
        WHERE drone_id = $1 AND ended_at IS NULL
        ORDER BY started_at DESC
        LIMIT 1
        "#,
    )
    .bind(drone_id)
    .fetch_optional(pool)
    .await?;
    Ok(flight)
}

async fn cache_latest_status(state: &AppState, telemetry: &Telemetry) -> anyhow::Result<()> {
    let mut conn = state.redis.get_multiplexed_async_connection().await?;
    let key = format!("neurovector:drone:{}:status", telemetry.drone_id);
    let payload = json!({
        "online": true,
        "latest": telemetry,
        "updated_at": Utc::now(),
    });

    let _: () = conn
        .set_ex(
            key,
            serde_json::to_string(&payload)?,
            state.telemetry_online_ttl_seconds,
        )
        .await?;
    Ok(())
}
