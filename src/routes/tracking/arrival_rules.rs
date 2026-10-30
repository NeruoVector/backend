use chrono::{DateTime, Duration, Utc};

use crate::geo;
use crate::models::AltitudeReference;

const ARRIVAL_RADIUS_METERS: f64 = 3.0;
const ARRIVAL_MAX_SPEED_MPS: f64 = 1.0;
pub(super) const ARRIVAL_HOLD_SECONDS: i64 = 10;
pub(super) const ARRIVAL_SAMPLE_MAX_GAP_SECONDS: i64 = 6;
pub(super) const TELEMETRY_FRESH_SECONDS: i64 = 30;

#[derive(Debug, Clone, Copy)]
pub(super) struct TelemetrySample {
    pub(super) device_ts: DateTime<Utc>,
    pub(super) received_at: DateTime<Utc>,
    pub(super) latitude: f64,
    pub(super) longitude: f64,
    pub(super) speed_mps: Option<f64>,
    pub(super) altitude_m: Option<f64>,
    pub(super) vertical_speed_mps: Option<f64>,
    pub(super) altitude_reference: Option<AltitudeReference>,
}

pub(super) struct ArrivalTarget {
    pub(super) latitude: f64,
    pub(super) longitude: f64,
    pub(super) altitude_m: f64,
    pub(super) reference: Option<AltitudeReference>,
}

pub(super) fn arrival_hold_is_met(
    samples: &[TelemetrySample],
    destination: &ArrivalTarget,
    now: DateTime<Utc>,
) -> bool {
    if destination.reference != Some(AltitudeReference::HomeRelative)
        || !destination.altitude_m.is_finite()
    {
        return false;
    }
    let Some(last) = samples.last() else {
        return false;
    };
    if now.signed_duration_since(last.received_at) > Duration::seconds(5) || last.received_at > now
    {
        return false;
    }
    if now.signed_duration_since(last.device_ts) > Duration::seconds(5) || last.device_ts > now {
        return false;
    }
    let Some(first) = samples.first() else {
        return false;
    };
    if samples.len() < 2 {
        return false;
    }
    if last.device_ts - first.device_ts < Duration::seconds(ARRIVAL_HOLD_SECONDS)
        || last.received_at - first.received_at < Duration::seconds(ARRIVAL_HOLD_SECONDS)
    {
        return false;
    }

    let mut previous_device_ts = first.device_ts;
    let mut previous_received_at = first.received_at;
    for sample in samples {
        let Some(speed) = sample.speed_mps else {
            return false;
        };
        let Some(altitude) = sample.altitude_m else {
            return false;
        };
        let Some(vertical_speed) = sample.vertical_speed_mps else {
            return false;
        };
        if sample.altitude_reference != destination.reference
            || !altitude.is_finite()
            || (altitude - destination.altitude_m).abs() > 1.0
            || !vertical_speed.is_finite()
            || vertical_speed.abs() > 0.5
            || !speed.is_finite()
            || speed.abs() > ARRIVAL_MAX_SPEED_MPS
            || !valid_coordinate(sample.latitude, sample.longitude)
            || geo::haversine_m(
                (destination.latitude, destination.longitude),
                (sample.latitude, sample.longitude),
            ) > ARRIVAL_RADIUS_METERS
        {
            return false;
        }
        if sample.device_ts < previous_device_ts
            || sample.received_at < previous_received_at
            || sample.device_ts - previous_device_ts
                > Duration::seconds(ARRIVAL_SAMPLE_MAX_GAP_SECONDS)
            || sample.received_at - previous_received_at
                > Duration::seconds(ARRIVAL_SAMPLE_MAX_GAP_SECONDS)
        {
            return false;
        }
        previous_device_ts = sample.device_ts;
        previous_received_at = sample.received_at;
    }
    true
}

pub(super) fn telemetry_is_fresh(
    device_ts: Option<DateTime<Utc>>,
    received_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    max_age: Duration,
) -> bool {
    let is_recent = |timestamp: DateTime<Utc>| {
        let age = now.signed_duration_since(timestamp);
        age >= Duration::zero() && age <= max_age
    };
    device_ts.is_some_and(is_recent) && received_at.is_some_and(is_recent)
}

pub(super) fn valid_coordinate(latitude: f64, longitude: f64) -> bool {
    latitude.is_finite()
        && longitude.is_finite()
        && (-90.0..=90.0).contains(&latitude)
        && (-180.0..=180.0).contains(&longitude)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(at: DateTime<Utc>, latitude: f64, speed_mps: f64) -> TelemetrySample {
        TelemetrySample {
            device_ts: at,
            received_at: at,
            latitude,
            longitude: 127.0,
            speed_mps: Some(speed_mps),
            altitude_m: Some(8.0),
            vertical_speed_mps: Some(0.0),
            altitude_reference: Some(AltitudeReference::HomeRelative),
        }
    }

    #[test]
    fn arrival_requires_fresh_stationary_samples_through_full_hold_window() {
        // Given fresh low-speed telemetry covering the hold window, When evaluating arrival, Then the geofence condition is met.
        let now = Utc::now();
        let points = vec![
            sample(now - Duration::seconds(12), 37.5, 0.2),
            sample(now - Duration::seconds(6), 37.50001, 0.3),
            sample(now, 37.5, 0.2),
        ];
        assert!(arrival_hold_is_met(&points, &target(), now));
    }

    #[test]
    fn arrival_rejects_fast_or_stale_telemetry() {
        // Given fast or stale telemetry, When evaluating arrival, Then neither sample sequence can notify the customer.
        let now = Utc::now();
        let fast = vec![
            sample(now - Duration::seconds(12), 37.5, 0.2),
            sample(now - Duration::seconds(6), 37.5, 2.0),
            sample(now, 37.5, 0.2),
        ];
        assert!(!arrival_hold_is_met(&fast, &target(), now));

        let stale = vec![
            sample(now - Duration::seconds(25), 37.5, 0.2),
            sample(now - Duration::seconds(15), 37.5, 0.2),
        ];
        assert!(!arrival_hold_is_met(&stale, &target(), now));
    }

    #[test]
    fn delayed_device_telemetry_cannot_complete_arrival() {
        // Given old device timestamps received now, When evaluating arrival, Then backfilled points do not notify the customer.
        let now = Utc::now();
        let old = now - Duration::hours(1);
        let points = vec![
            TelemetrySample {
                device_ts: old - Duration::seconds(12),
                received_at: now - Duration::seconds(12),
                latitude: 37.5,
                longitude: 127.0,
                speed_mps: Some(0.1),
                altitude_m: Some(8.0),
                vertical_speed_mps: Some(0.0),
                altitude_reference: Some(AltitudeReference::HomeRelative),
            },
            TelemetrySample {
                device_ts: old,
                received_at: now,
                latitude: 37.5,
                longitude: 127.0,
                speed_mps: Some(0.1),
                altitude_m: Some(8.0),
                vertical_speed_mps: Some(0.0),
                altitude_reference: Some(AltitudeReference::HomeRelative),
            },
        ];

        assert!(!arrival_hold_is_met(&points, &target(), now));
    }

    fn target() -> ArrivalTarget {
        ArrivalTarget {
            latitude: 37.5,
            longitude: 127.0,
            altitude_m: 8.0,
            reference: Some(AltitudeReference::HomeRelative),
        }
    }

    #[test]
    fn tracking_telemetry_freshness_requires_device_and_receive_time() {
        // Given delayed or current samples, When checking freshness, Then both source and server times must be current.
        let now = Utc::now();
        let age = Duration::seconds(TELEMETRY_FRESH_SECONDS);
        assert!(telemetry_is_fresh(Some(now), Some(now), now, age));
        assert!(!telemetry_is_fresh(
            Some(now - Duration::hours(1)),
            Some(now),
            now,
            age,
        ));
    }

    #[test]
    fn arrival_rejects_wrong_height_reference_and_vertical_motion() {
        let now = Utc::now();
        let mut points = vec![
            sample(now - Duration::seconds(12), 37.5, 0.2),
            sample(now - Duration::seconds(6), 37.5, 0.2),
            sample(now, 37.5, 0.2),
        ];
        let target = ArrivalTarget {
            latitude: 37.5,
            longitude: 127.0,
            altitude_m: 8.0,
            reference: Some(AltitudeReference::HomeRelative),
        };
        assert!(arrival_hold_is_met(&points, &target, now));
        points[1].altitude_m = Some(2.4);
        assert!(!arrival_hold_is_met(&points, &target, now));
        points[1].altitude_m = Some(8.0);
        points[1].altitude_reference = None;
        assert!(!arrival_hold_is_met(&points, &target, now));
        points[1].altitude_reference = Some(AltitudeReference::HomeRelative);
        points[1].vertical_speed_mps = Some(0.6);
        assert!(!arrival_hold_is_met(&points, &target, now));
    }

    #[test]
    fn arrival_rejects_missing_altitude_unknown_plan_and_nearby_stop() {
        let now = Utc::now();
        let mut points = vec![
            sample(now - Duration::seconds(12), 37.5, 0.0),
            sample(now - Duration::seconds(6), 37.5, 0.0),
            sample(now, 37.5, 0.0),
        ];
        let mut destination = target();
        destination.reference = None;
        assert!(!arrival_hold_is_met(&points, &destination, now));
        destination.reference = Some(AltitudeReference::HomeRelative);
        points[1].altitude_m = None;
        assert!(!arrival_hold_is_met(&points, &destination, now));
        points[1].altitude_m = Some(8.0);
        points[1].altitude_reference = Some(AltitudeReference::MeanSeaLevel);
        assert!(!arrival_hold_is_met(&points, &destination, now));
        points[1].altitude_reference = Some(AltitudeReference::HomeRelative);
        points[1].latitude = 37.5001;
        assert!(!arrival_hold_is_met(&points, &destination, now));
    }
}
