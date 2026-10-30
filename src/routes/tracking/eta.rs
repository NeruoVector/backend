use crate::db::PgPool;
use chrono::{DateTime, Duration, Utc};
use uuid::Uuid;

use crate::{error::AppError, geo};

use super::types::TrackingRow;

#[derive(Debug)]
struct RouteStop {
    latitude: Option<f64>,
    longitude: Option<f64>,
}
crate::db::row!(RouteStop {
    latitude,
    longitude
});

pub(super) async fn telemetry_eta(
    pool: &PgPool,
    row: &TrackingRow,
    delivery_id: Uuid,
    stop_sequence: i32,
    now: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>, AppError> {
    let (Some(latitude), Some(longitude), Some(telemetry_speed)) = (
        row.telemetry_latitude,
        row.telemetry_longitude,
        row.telemetry_speed_mps,
    ) else {
        return Ok(None);
    };
    let stops = crate::db::fetch_all::<RouteStop>(
        &pool.get().await?,
        r#"
        SELECT o.latitude, o.longitude
        FROM neurovector.delivery_stops s
        JOIN neurovector.orders o ON o.id = s.order_id
        WHERE s.delivery_id = $1
          AND s.stop_sequence <= $2
          AND s.status = 'scheduled'
        ORDER BY s.stop_sequence
        "#,
        &[&(delivery_id), &(stop_sequence)],
    )
    .await?;
    let mut current = (latitude, longitude);
    let mut remaining_distance = 0.0;
    for stop in stops {
        let Some(destination) = stop.latitude.zip(stop.longitude) else {
            return Ok(None);
        };
        remaining_distance += geo::haversine_m(current, destination);
        current = destination;
    }
    let configured_speed = row.cruise_speed_mps.unwrap_or(8.0);
    let speed = if telemetry_speed.is_finite() && telemetry_speed > 0.5 {
        telemetry_speed.min(configured_speed.max(0.5))
    } else {
        configured_speed.max(0.5)
    };
    let seconds = remaining_distance / speed;
    let duration = std::time::Duration::try_from_secs_f64(seconds)
        .map_err(|_| AppError::ServiceUnavailable("ETA duration is invalid".into()))?;
    let duration = Duration::from_std(duration)
        .map_err(|_| AppError::ServiceUnavailable("ETA duration is invalid".into()))?;
    Ok(now.checked_add_signed(duration))
}
