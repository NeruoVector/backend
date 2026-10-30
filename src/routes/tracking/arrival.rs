use chrono::{DateTime, Utc};
use deadpool_postgres::Transaction;
use uuid::Uuid;

use super::arrival_rules::{ArrivalTarget, TelemetrySample, arrival_hold_is_met};
use crate::{db::PgPool, error::AppError, mission::PlanPayload, models::AltitudeReference};

#[derive(Debug)]
struct ArrivalSample {
    id: i64,
    current_id: i64,
    device_ts: DateTime<Utc>,
    received_at: DateTime<Utc>,
    latitude: Option<f64>,
    longitude: Option<f64>,
    altitude_m: Option<f64>,
    speed_mps: Option<f64>,
    vertical_speed_mps: Option<f64>,
    reference: Option<String>,
}
crate::db::row!(ArrivalSample {
    id,
    current_id,
    device_ts,
    received_at,
    latitude,
    longitude,
    altitude_m,
    speed_mps,
    vertical_speed_mps,
    reference
});

struct StopPlan {
    latitude: f64,
    longitude: f64,
    payload: serde_json::Value,
}
crate::db::row!(StopPlan {
    latitude,
    longitude,
    payload
});

/// Re-evaluate a particular stop using the active plan and the latest accepted flight telemetry.
pub(crate) async fn handoff_pose_is_valid(
    tx: &Transaction<'_>,
    order_id: Uuid,
) -> Result<bool, AppError> {
    let Some(StopPlan { latitude: lat, longitude: lon, payload }) = crate::db::fetch_optional::<StopPlan>(
        tx, r#"SELECT o.latitude, o.longitude, p.payload
        FROM neurovector.orders o JOIN neurovector.delivery_stops s ON s.order_id=o.id AND s.delivery_id=o.delivery_id
        JOIN neurovector.deliveries d ON d.id=s.delivery_id
        JOIN neurovector.mission_plans p ON p.route_id=d.route_id AND p.status='verified'
        WHERE o.id=$1 AND o.status='in_transit' AND s.status IN ('scheduled','arrived')
          AND d.status IN ('dispatched','in_flight') AND o.latitude IS NOT NULL AND o.longitude IS NOT NULL
          AND NOT EXISTS (SELECT 1 FROM neurovector.delivery_stops previous WHERE previous.delivery_id=s.delivery_id AND previous.stop_sequence<s.stop_sequence AND previous.status IN ('scheduled','arrived')) FOR SHARE OF p"#, &[&order_id]).await? else { return Ok(false); };
    let Ok(plan) = serde_json::from_value::<PlanPayload>(payload) else {
        return Ok(false);
    };
    let Some(altitude_m) = resolve_target_altitude(&plan.required_stops, (lat, lon)) else {
        return Ok(false);
    };
    let target = ArrivalTarget {
        latitude: lat,
        longitude: lon,
        altitude_m,
        reference: plan.altitude_reference,
    };
    let rows = crate::db::fetch_all::<ArrivalSample>(tx, r#"SELECT t.id, current.id AS current_id, t.ts AS device_ts, t.received_at, t.latitude, t.longitude, t.altitude_m, t.speed_mps, t.vertical_speed_mps, t.raw->>'altitude_reference' AS reference
        FROM neurovector.orders o JOIN neurovector.delivery_stops s ON s.order_id=o.id AND s.delivery_id=o.delivery_id
        JOIN neurovector.deliveries d ON d.id=s.delivery_id
        JOIN neurovector.flight_sessions fs ON fs.delivery_id=d.id AND fs.drone_id=d.drone_id AND fs.ended_at IS NULL
        JOIN neurovector.telemetry t ON t.flight_id=fs.id AND t.drone_id=d.drone_id AND t.accepted_for_current_state
        JOIN neurovector.current_drone_telemetry current ON current.drone_id=d.drone_id
        WHERE o.id=$1 AND t.received_at >= GREATEST(now()-interval '16 seconds', COALESCE((SELECT max(previous.completed_at) FROM neurovector.delivery_stops previous WHERE previous.delivery_id=s.delivery_id AND previous.stop_sequence<s.stop_sequence AND previous.status='completed'), d.started_at, d.scheduled_at))
          AND t.ts >= GREATEST(now()-interval '16 seconds', COALESCE((SELECT max(previous.completed_at) FROM neurovector.delivery_stops previous WHERE previous.delivery_id=s.delivery_id AND previous.stop_sequence<s.stop_sequence AND previous.status='completed'), d.started_at, d.scheduled_at))
        ORDER BY t.ts, t.received_at, t.id"#, &[&order_id]).await?;
    if rows.last().is_none_or(|r| r.id != r.current_id) {
        return Ok(false);
    }
    let samples: Vec<_> = rows
        .iter()
        .map(|r| TelemetrySample {
            device_ts: r.device_ts,
            received_at: r.received_at,
            latitude: r.latitude.unwrap_or(f64::NAN),
            longitude: r.longitude.unwrap_or(f64::NAN),
            altitude_m: r.altitude_m,
            speed_mps: r.speed_mps,
            vertical_speed_mps: r.vertical_speed_mps,
            altitude_reference: match r.reference.as_deref() {
                Some("home_relative") => Some(AltitudeReference::HomeRelative),
                Some("mean_sea_level") => Some(AltitudeReference::MeanSeaLevel),
                _ => None,
            },
        })
        .collect();
    Ok(arrival_hold_is_met(&samples, &target, Utc::now()))
}

fn resolve_target_altitude(
    points: &[crate::geo::Waypoint],
    destination: (f64, f64),
) -> Option<f64> {
    let mut matching = points.iter().filter(|point| {
        (point.lat - destination.0).abs() < 1e-10 && (point.lon - destination.1).abs() < 1e-10
    });
    let first = matching.next()?;
    matching
        .all(|point| {
            point.lat == first.lat && point.lon == first.lon && point.altitude_m == first.altitude_m
        })
        .then_some(first.altitude_m)
}

/// Serialize with ingest and activation before taking delivery or stop row locks.
pub(crate) async fn lock_delivery_pose(
    tx: &Transaction<'_>,
    delivery_id: Uuid,
) -> Result<bool, AppError> {
    let Some(drone) = crate::db::scalar_optional::<String>(
        tx,
        "SELECT drone_id FROM neurovector.deliveries WHERE id=$1",
        &[&delivery_id],
    )
    .await?
    else {
        return Ok(false);
    };
    crate::db::execute(
        tx,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        &[&drone],
    )
    .await?;
    let locked = crate::db::scalar_optional::<String>(
        tx,
        "SELECT drone_id FROM neurovector.deliveries WHERE id=$1 FOR UPDATE",
        &[&delivery_id],
    )
    .await?;
    Ok(locked.as_deref() == Some(drone.as_str()))
}

/// Record only arrivals whose current accepted pose satisfies the handoff gate.
pub async fn record_arrival_events(pool: &PgPool) -> Result<usize, AppError> {
    let mut connection = pool.get().await?;
    let stops = crate::db::fetch_all::<(Uuid, Uuid, Uuid)>(&connection, "SELECT s.id,s.order_id,s.delivery_id FROM neurovector.delivery_stops s JOIN neurovector.deliveries d ON d.id=s.delivery_id WHERE s.status='scheduled' AND d.status IN ('dispatched','in_flight')", &[]).await?;
    let mut recorded = 0;
    for (stop_id, order_id, delivery_id) in stops {
        let tx = connection.transaction().await?;
        if !lock_delivery_pose(&tx, delivery_id).await? {
            continue;
        }
        let stop = crate::db::scalar_optional::<Uuid>(&tx, "SELECT id FROM neurovector.delivery_stops WHERE id=$1 AND order_id=$2 AND delivery_id=$3 AND status='scheduled' FOR UPDATE", &[&stop_id,&order_id,&delivery_id]).await?;
        if stop.is_none() {
            continue;
        }
        if !handoff_pose_is_valid(&tx, order_id).await? {
            continue;
        }
        let changed = crate::db::execute(&tx, "UPDATE neurovector.delivery_stops SET status='arrived', arrived_at=now(), updated_at=now() WHERE id=$1 AND status='scheduled'", &[&stop_id]).await?;
        if changed == 0 {
            continue;
        }
        crate::db::execute(&tx, "INSERT INTO neurovector.order_tracking_events(order_id,delivery_stop_id,event_type,dedupe_key,payload) VALUES($1,$2,'arrived','arrival',jsonb_build_object('source','telemetry','notification_id_is_stable',true)) ON CONFLICT(order_id,dedupe_key) DO NOTHING", &[&order_id,&stop_id]).await?;
        tx.commit().await?;
        recorded += 1;
    }
    Ok(recorded)
}

#[cfg(test)]
mod target_tests {
    use super::*;
    use crate::geo::Waypoint;

    fn waypoint(lat: f64, altitude_m: f64) -> Waypoint {
        Waypoint {
            lat,
            lon: 127.0,
            altitude_m,
        }
    }

    #[test]
    fn repeated_identical_targets_share_the_same_arrival_height() {
        let points = vec![
            waypoint(37.5, 8.0),
            waypoint(37.5, 8.0),
            waypoint(37.5, 8.0),
        ];
        assert_eq!(resolve_target_altitude(&points, (37.5, 127.0)), Some(8.0));
    }

    #[test]
    fn conflicting_targets_are_ambiguous() {
        let points = vec![waypoint(37.5, 8.0), waypoint(37.5, 9.0)];
        assert_eq!(resolve_target_altitude(&points, (37.5, 127.0)), None);
        let near_but_distinct = vec![waypoint(37.5, 8.0), waypoint(37.5 + 1e-11, 8.0)];
        assert_eq!(
            resolve_target_altitude(&near_but_distinct, (37.5, 127.0)),
            None
        );
    }

    #[test]
    fn missing_customer_target_is_rejected() {
        let points = vec![waypoint(37.6, 8.0)];
        assert_eq!(resolve_target_altitude(&points, (37.5, 127.0)), None);
    }
}
