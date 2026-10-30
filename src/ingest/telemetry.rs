use crate::{
    error::AppError,
    models::{FlightSession, Telemetry, TelemetryMessage},
    state::AppState,
};
use chrono::Utc;
use serde_json::json;

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

    let mut client = state.pool.get().await?;
    let tx = client.transaction().await?;
    crate::db::execute(
        &tx,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        &[&drone_id],
    )
    .await?;
    let now: chrono::DateTime<Utc> =
        crate::db::scalar_one(&tx, "SELECT clock_timestamp()", &[]).await?;
    let ts = chrono::DateTime::from_timestamp_micros(body.ts.unwrap_or(now).timestamp_micros())
        .ok_or_else(|| AppError::BadRequest("invalid telemetry timestamp".into()))?;
    let ttl = i64::try_from(state.telemetry_online_ttl_seconds).unwrap_or(i64::MAX);
    let latest = crate::db::scalar_optional::<chrono::DateTime<Utc>>(
        &tx,
        "SELECT ts FROM neurovector.current_drone_telemetry WHERE drone_id = $1",
        &[&drone_id],
    )
    .await?;
    let age = now.signed_duration_since(ts);
    let eligible = age >= chrono::Duration::seconds(-5)
        && age <= chrono::Duration::seconds(ttl)
        && latest.is_none_or(|previous| ts > previous);
    let active = if eligible {
        find_active_flight(&tx, &drone_id).await?
    } else {
        None
    };

    let flight = match (active, terminal, starting && eligible) {
        (Some(flight), true, _) => {
            let close_status = match status.as_str() {
                "failed" => "failed",
                "aborted" => "aborted",
                _ => "completed",
            };
            let closed = crate::db::fetch_one::<FlightSession>(
                &tx,
                r#"
                UPDATE neurovector.flight_sessions
                SET ended_at = now(), status = $2
                WHERE id = $1
                RETURNING id, drone_id, delivery_id, status, started_at, ended_at, created_at
                "#,
                &[&(flight.id), &(close_status)],
            )
            .await?;
            tracing::info!(drone_id = %drone_id, flight_id = %closed.id, "flight session closed");
            Some(closed)
        }
        (Some(flight), false, _) => Some(flight),
        (None, false, true) => {
            let delivery_id = crate::db::scalar_optional::<uuid::Uuid>(
                &tx,
                r#"
                SELECT id FROM neurovector.deliveries
                WHERE drone_id = $1 AND status IN ('dispatched', 'in_flight')
                ORDER BY scheduled_at DESC
                LIMIT 1
                "#,
                &[&(&drone_id)],
            )
            .await?;

            let created = crate::db::fetch_one::<FlightSession>(
                &tx,
                r#"
                INSERT INTO neurovector.flight_sessions (drone_id, delivery_id, status)
                VALUES ($1, $2, 'in_flight')
                RETURNING id, drone_id, delivery_id, status, started_at, ended_at, created_at
                "#,
                &[&(&drone_id), &(delivery_id)],
            )
            .await?;
            tracing::info!(drone_id = %drone_id, flight_id = %created.id, "flight session started");
            Some(created)
        }
        (None, _, _) => None,
    };

    let raw = serde_json::to_value(&body).unwrap_or_else(|_| json!({}));
    let telemetry = crate::db::fetch_one::<Telemetry>(
        &tx,
        r#"
        INSERT INTO neurovector.telemetry
            (drone_id, flight_id, ts, latitude, longitude, altitude_m, speed_mps,
             vertical_speed_mps, battery_pct, heading_deg, status, raw, received_at,
             accepted_for_current_state)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
        RETURNING id, drone_id, flight_id, ts, latitude, longitude, altitude_m, speed_mps,
                  vertical_speed_mps, battery_pct, heading_deg, status, received_at
        "#,
        &[
            &(&drone_id),
            &(flight.as_ref().map(|f| f.id)),
            &(ts),
            &(body.latitude),
            &(body.longitude),
            &(body.altitude_m),
            &(body.speed_mps),
            &(body.vertical_speed_mps),
            &(body.battery_pct),
            &(body.heading_deg),
            &(body.status.as_deref()),
            &(raw),
            &(now),
            &(eligible),
        ],
    )
    .await?;

    tx.commit().await?;
    if eligible && let Err(err) = cache_latest_status(state, &telemetry).await {
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
    client: &impl deadpool_postgres::GenericClient,
    drone_id: &str,
) -> Result<Option<FlightSession>, AppError> {
    let flight = crate::db::fetch_optional::<FlightSession>(
        client,
        r#"
        SELECT id, drone_id, delivery_id, status, started_at, ended_at, created_at
        FROM neurovector.flight_sessions
        WHERE drone_id = $1 AND ended_at IS NULL
        ORDER BY started_at DESC
        LIMIT 1
        "#,
        &[&(drone_id)],
    )
    .await?;
    Ok(flight)
}

async fn cache_latest_status(state: &AppState, telemetry: &Telemetry) -> anyhow::Result<()> {
    let key = format!("neurovector:drone:{}:status", telemetry.drone_id);
    let payload = json!({
        "online": true,
        "version": telemetry.id.to_string(),
        "latest": telemetry,
        "updated_at": Utc::now(),
    });

    let payload = serde_json::to_string(&payload)?;
    let ttl_ms = i64::try_from(state.telemetry_online_ttl_seconds)
        .unwrap_or(i64::MAX / 1000)
        .saturating_mul(1000);
    let deadline = telemetry
        .ts
        .min(telemetry.received_at)
        .timestamp_millis()
        .saturating_add(ttl_ms);
    // A delayed post-commit writer must never replace a newer row.
    state
        .redis
        .execute(|mut conn| async move {
            redis::cmd("EVAL")
                .arg(
                    r#"
            local time = redis.call('TIME')
            local remaining = tonumber(ARGV[3]) - (time[1] * 1000 + math.floor(time[2] / 1000))
            if remaining <= 0 then return 0 end
            local old = redis.call('GET', KEYS[1])
            local new = ARGV[1]
            if old then
                local ok, value = pcall(cjson.decode, old)
                local version = ok and value.version
                if type(version) == 'string' and (#version > #new or (#version == #new and version >= new)) then return 0 end
            end
            redis.call('PSETEX', KEYS[1], remaining, ARGV[2])
            return 1
        "#,
                )
                .arg(1)
                .arg(key)
                .arg(telemetry.id)
                .arg(payload)
                .arg(deadline)
                .query_async::<i64>(&mut conn)
                .await
        })
        .await?;
    Ok(())
}
