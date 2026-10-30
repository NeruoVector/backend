use super::*;
use crate::{geo::Waypoint, mission::Constraints};

fn point(lat: f64, altitude_m: f64) -> Waypoint {
    Waypoint {
        lat,
        lon: 127.0,
        altitude_m,
    }
}

fn payload(points: Vec<Waypoint>) -> PlanPayload {
    PlanPayload {
        altitude_reference: None,
        selected_index: 0,
        required_stops: points,
        candidates: vec![],
        validations: vec![],
        constraints: Constraints {
            max_altitude_m: 30.0,
            max_distance_m: 20_000.0,
            max_leg_m: 5_000.0,
            speed_mps: 5.0,
            power_w: 120.0,
            battery_wh: 500.0,
            hold_seconds: 0.0,
        },
        zones: vec![],
        window_target: None,
        optimality_scope: String::new(),
    }
}

fn stop(lat: f64, sequence: i32, status: &str) -> StopRow {
    StopRow {
        order_id: Uuid::new_v4(),
        stop_sequence: sequence,
        status: status.into(),
        latitude: Some(lat),
        longitude: Some(127.0),
        order_status: "in_transit".into(),
    }
}

#[test]
fn activation_preserves_remaining_stop_order_and_original_home() {
    let old = payload(vec![
        point(37.0, 8.0),
        point(37.001, 8.0),
        point(37.002, 8.0),
        point(37.0, 8.0),
    ]);
    let stops = vec![stop(37.001, 1, "completed"), stop(37.002, 2, "scheduled")];
    let good = payload(vec![
        point(37.0005, 8.0),
        point(37.002, 8.0),
        point(37.0, 8.0),
    ]);
    let omitted = payload(vec![point(37.0005, 8.0), point(37.0, 8.0)]);
    let wrong_home = payload(vec![
        point(37.0005, 8.0),
        point(37.002, 8.0),
        point(37.003, 8.0),
    ]);
    let wrong_altitude = payload(vec![
        point(37.0005, 8.0),
        point(37.002, 20.0),
        point(37.0, 8.0),
    ]);
    assert!(remaining_stops_match(&old, &good, &stops, true));
    assert!(!remaining_stops_match(&old, &omitted, &stops, true));
    assert!(!remaining_stops_match(&old, &wrong_home, &stops, true));
    assert!(!remaining_stops_match(&old, &wrong_altitude, &stops, true));
    let duplicate_old = payload(vec![
        point(37.0, 8.0),
        point(37.001, 8.0),
        point(37.001, 20.0),
        point(37.0, 8.0),
    ]);
    let duplicate_stops = vec![stop(37.001, 1, "completed"), stop(37.001, 2, "scheduled")];
    let duplicate_correct = payload(vec![
        point(37.0005, 8.0),
        point(37.001, 20.0),
        point(37.0, 8.0),
    ]);
    let duplicate_wrong = payload(vec![
        point(37.0005, 8.0),
        point(37.001, 8.0),
        point(37.0, 8.0),
    ]);
    assert!(!remaining_stops_match(
        &duplicate_old,
        &duplicate_correct,
        &duplicate_stops,
        true
    ));
    assert!(!remaining_stops_match(
        &duplicate_old,
        &duplicate_wrong,
        &duplicate_stops,
        true
    ));
    let both_pending = vec![stop(37.001, 1, "scheduled"), stop(37.002, 2, "scheduled")];
    let reversed = payload(vec![
        point(37.0005, 8.0),
        point(37.002, 8.0),
        point(37.001, 8.0),
        point(37.0, 8.0),
    ]);
    assert!(!remaining_stops_match(&old, &reversed, &both_pending, true));
    let mut window_old = old.clone();
    window_old.window_target = Some(crate::mission::WindowTarget {
        window: point(37.002, 8.0),
        outward_bearing_deg: 0.0,
        standoff_m: 2.0,
        approach_distance_m: 1.0,
        hold_seconds: 3.0,
    });
    let mut window_new = good.clone();
    window_new.window_target = window_old.window_target.clone();
    assert!(remaining_stops_match(
        &window_old,
        &window_new,
        &stops,
        true
    ));
    window_new.window_target.as_mut().unwrap().hold_seconds = 4.0;
    assert!(!remaining_stops_match(
        &window_old,
        &window_new,
        &stops,
        true
    ));
    assert!(!remaining_stops_match(&old, &window_new, &stops, true));
}

#[test]
fn activation_requires_fresh_held_pose_and_available_energy() {
    let now = Utc::now();
    let plan = payload(vec![point(37.0, 8.0), point(37.001, 8.0)]);
    let mut telemetry = Telemetry {
        id: 1,
        drone_id: "test".into(),
        flight_id: Some(Uuid::new_v4()),
        ts: now,
        received_at: now,
        latitude: Some(37.0),
        longitude: Some(127.0),
        altitude_m: Some(8.0),
        speed_mps: Some(0.2),
        vertical_speed_mps: Some(0.1),
        battery_pct: Some(50.0),
        heading_deg: None,
        status: Some("flying".into()),
    };
    assert!(valid_pose(&telemetry, &plan, 100.0, now));
    telemetry.status = Some("taking_off".into());
    assert!(!valid_pose(&telemetry, &plan, 100.0, now));
    telemetry.status = Some("flying".into());
    assert!(!valid_pose(&telemetry, &plan, 201.0, now));
    telemetry.vertical_speed_mps = None;
    assert!(!valid_pose(&telemetry, &plan, 100.0, now));
    telemetry.vertical_speed_mps = Some(f64::NAN);
    assert!(!valid_pose(&telemetry, &plan, 100.0, now));
    telemetry.vertical_speed_mps = Some(f64::INFINITY);
    assert!(!valid_pose(&telemetry, &plan, 100.0, now));
    telemetry.vertical_speed_mps = Some(-2.0);
    assert!(!valid_pose(&telemetry, &plan, 100.0, now));
    telemetry.vertical_speed_mps = Some(0.1);
    telemetry.speed_mps = Some(2.0);
    assert!(!valid_pose(&telemetry, &plan, 100.0, now));
    telemetry.speed_mps = Some(0.2);
    telemetry.ts = now - Duration::seconds(6);
    assert!(!valid_pose(&telemetry, &plan, 100.0, now));
    telemetry.ts = now;
    telemetry.latitude = Some(37.001);
    assert!(!valid_pose(&telemetry, &plan, 100.0, now));
}
