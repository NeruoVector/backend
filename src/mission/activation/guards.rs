use super::*;

pub(super) const MAX_POSE_AGE_SECONDS: i64 = 5;
const MAX_START_DISTANCE_M: f64 = 2.0;
const MAX_START_ALTITUDE_DIFF_M: f64 = 3.0;
const MAX_PAYLOAD_AGE_SECONDS: i64 = 300;
const ENERGY_FRACTION: f64 = 0.8;

pub(super) fn fresh(at: DateTime<Utc>, now: DateTime<Utc>, max_age: i64) -> bool {
    let age = now.signed_duration_since(at);
    age >= Duration::zero() && age <= Duration::seconds(max_age)
}
pub(super) fn valid_measurement(
    measurement: Option<(i32, DateTime<Utc>)>,
    capacity: i32,
    now: DateTime<Utc>,
) -> bool {
    measurement.is_some_and(|(weight, measured_at)| {
        weight > 0 && weight <= capacity && fresh(measured_at, now, MAX_PAYLOAD_AGE_SECONDS)
    })
}
pub(super) fn valid_pose(
    t: &Telemetry,
    plan: &PlanPayload,
    energy: f64,
    now: DateTime<Utc>,
) -> bool {
    let Some(vertical_speed) = t.vertical_speed_mps else {
        return false;
    };
    let Some((lat, lon, alt, speed, battery, start)) = t
        .latitude
        .zip(t.longitude)
        .zip(t.altitude_m)
        .zip(t.speed_mps)
        .zip(t.battery_pct)
        .zip(plan.required_stops.first())
        .map(|(((((lat, lon), alt), speed), battery), start)| {
            (lat, lon, alt, speed, battery, start)
        })
    else {
        return false;
    };
    let flying = t.status.as_deref().is_some_and(|s| {
        matches!(
            s.to_ascii_lowercase().as_str(),
            "flying" | "in_flight" | "mission"
        )
    });
    fresh(t.ts, now, MAX_POSE_AGE_SECONDS)
        && fresh(t.received_at, now, MAX_POSE_AGE_SECONDS)
        && flying
        && speed.is_finite()
        && (0.0..=1.0).contains(&speed)
        && vertical_speed.is_finite()
        && vertical_speed.abs() <= 1.0
        && battery.is_finite()
        && (0.0..=100.0).contains(&battery)
        && energy.is_finite()
        && energy >= 0.0
        && energy <= plan.constraints.battery_wh * battery / 100.0 * ENERGY_FRACTION
        && geo::haversine_m((lat, lon), (start.lat, start.lon)) <= MAX_START_DISTANCE_M
        && (alt - start.altitude_m).abs() <= MAX_START_ALTITUDE_DIFF_M
}
pub(super) fn remaining_stops_match(
    old: &PlanPayload,
    new: &PlanPayload,
    stops: &[StopRow],
    automatic: bool,
) -> bool {
    if automatic && stops.is_empty() {
        return false;
    }
    let Some((old_home, new_home)) = old.required_stops.last().zip(new.required_stops.last())
    else {
        return false;
    };
    if !same(old_home, new_home) {
        return false;
    }
    if serde_json::to_value(&old.window_target).ok()
        != serde_json::to_value(&new.window_target).ok()
    {
        return false;
    }
    // Equal customer coordinates cannot unambiguously identify original altitude
    // after earlier stops complete; refuse replacement instead of guessing.
    for (index, stop) in stops.iter().enumerate() {
        let Some((lat, lon)) = stop.latitude.zip(stop.longitude) else {
            return false;
        };
        if stops[index + 1..].iter().any(|other| {
            other
                .latitude
                .zip(other.longitude)
                .is_some_and(|(other_lat, other_lon)| {
                    (lat - other_lat).abs() < 1e-10 && (lon - other_lon).abs() < 1e-10
                })
        }) {
            return false;
        }
    }
    let mut old_cursor = 1;
    let mut new_cursor = 1;
    let old_end = old
        .required_stops
        .len()
        .saturating_sub(usize::from(automatic));
    let new_end = new
        .required_stops
        .len()
        .saturating_sub(usize::from(automatic));
    for stop in stops
        .iter()
        .filter(|s| matches!(s.status.as_str(), "scheduled" | "arrived"))
    {
        if !matches!(
            stop.order_status.as_str(),
            "assigned" | "in_transit" | "delivered"
        ) {
            return false;
        }
        let Some((lat, lon)) = stop.latitude.zip(stop.longitude) else {
            return false;
        };
        let Some(old_index) = (old_cursor..old_end).find(|&i| {
            let point = &old.required_stops[i];
            ((point.lat - lat).abs() < 1e-10 && (point.lon - lon).abs() < 1e-10)
                || (i + 1 == old.required_stops.len()
                    && old.window_target.as_ref().is_some_and(|window| {
                        (window.window.lat - lat).abs() < 1e-10
                            && (window.window.lon - lon).abs() < 1e-10
                    }))
        }) else {
            return false;
        };
        let Some(new_index) = (new_cursor..new_end)
            .find(|&i| same(&old.required_stops[old_index], &new.required_stops[i]))
        else {
            return false;
        };
        old_cursor = old_index + 1;
        new_cursor = new_index + 1;
    }
    true
}
