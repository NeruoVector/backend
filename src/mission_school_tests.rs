use super::*;
use crate::state::test_state;
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use std::{fs, path::PathBuf};

fn number(value: &Value) -> f64 {
    value.as_f64().expect("numeric school fixture")
}
// shortcut: local ENU projection for this small dummy campus; use a geodesy model for larger maps.
fn point(fixture: &Value, xyz: [f64; 3]) -> Waypoint {
    let lat = number(&fixture["origin"]["lat"]);
    let lon = number(&fixture["origin"]["lon"]);
    Waypoint {
        lat: lat + (xyz[1] / 6_371_000.0).to_degrees(),
        lon: lon + (xyz[0] / (6_371_000.0 * lat.to_radians().cos())).to_degrees(),
        altitude_m: xyz[2],
    }
}
fn xyz(value: &Value) -> [f64; 3] {
    let values = value.as_array().expect("ENU array");
    assert_eq!(values.len(), 3);
    [number(&values[0]), number(&values[1]), number(&values[2])]
}
fn output_plan(
    directory: &std::path::Path,
    name: &str,
    state: &AppState,
    candidates: Vec<RouteInput>,
    zones: &[NoFlyZoneRow],
    window: Option<WindowTarget>,
) {
    let hold = window.as_ref().map_or(0.0, |w| w.hold_seconds);
    let validations: Vec<_> = candidates
        .iter()
        .map(|r| validate_route_with_zones(state, r, zones, hold))
        .collect();
    let selected_index = validations
        .iter()
        .enumerate()
        .filter(|(_, v)| v.valid)
        .min_by(|a, b| a.1.distance_m.total_cmp(&b.1.distance_m))
        .map(|(i, _)| i)
        .expect("at least one valid school candidate");
    let payload = PlanPayload {
        constraints: constraints(state, &candidates[selected_index], hold),
        selected_index,
        candidates,
        validations,
        zones: zones.to_vec(),
        window_target: window,
        optimality_scope: "synthetic school test candidates".into(),
    };
    let text = plan_text(
        Uuid::new_v4(),
        "school-drone-001",
        Utc::now() + Duration::seconds(600),
        &payload,
    )
    .unwrap();
    let report = json!({"synthetic": true, "name": name,
        "plan_text": text, "route_fingerprint": fnv(&text), "payload": payload});
    fs::write(
        directory.join(format!("{name}.json")),
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .unwrap();
}

#[tokio::test]
#[ignore = "run with sim/scripts/test-school.py and its explicit synthetic fixture"]
async fn school_fixture_routes_and_window() {
    let path = std::env::var("SCHOOL_FIXTURE_PATH").expect("SCHOOL_FIXTURE_PATH is required");
    let directory =
        PathBuf::from(std::env::var("SCHOOL_ARTIFACT_DIR").expect("SCHOOL_ARTIFACT_DIR required"));
    let fixture: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(fixture["synthetic"], true);
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://dummy:dummy@127.0.0.1:1/dummy")
        .unwrap();
    let mut state = test_state(pool);
    state.route_power_w = number(&fixture["drone"]["modeled_power_w"]);
    state.route_battery_wh = number(&fixture["drone"]["battery_wh"]);
    state.max_altitude_m = number(&fixture["drone"]["max_altitude_m"]);
    let zones: Vec<_> = fixture["buildings"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let c = xyz(&b["center_enu"]);
            let size = xyz(&b["size_m"]);
            let polygon: Vec<_> = [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)]
                .iter()
                .map(|(x, y)| {
                    point(
                        &fixture,
                        [c[0] + x * size[0] / 2., c[1] + y * size[1] / 2., 0.],
                    )
                })
                .collect();
            NoFlyZoneRow {
                id: Uuid::from_u128((i + 1) as u128),
                name: b["id"].as_str().unwrap().into(),
                polygon: serde_json::to_value(polygon).unwrap(),
                max_altitude_m: Some(number(&b["no_fly_ceiling_m"])),
            }
        })
        .collect();
    let start = point(&fixture, xyz(&fixture["route_start_enu"]));
    let end = point(&fixture, xyz(&fixture["outdoor_goal_enu"]));
    let direct = RouteInput {
        name: "dummy-school-outdoor".into(),
        drone_id: Some(fixture["drone"]["id"].as_str().unwrap().into()),
        waypoints: vec![start.clone(), end.clone()],
        cruise_speed_mps: Some(number(&fixture["drone"]["cruise_speed_mps"])),
        max_altitude_m: Some(state.max_altitude_m),
    };
    let blocked = validate_route_with_zones(&state, &direct, &zones, 0.);
    assert!(!blocked.valid, "direct route must hit the school building");
    assert!(!blocked.no_fly_zone_conflicts.is_empty());
    let path = generate_detour(&state, &start, &end, &zones, None)
        .unwrap()
        .expect("school detour");
    let mut detour = direct.clone();
    detour.waypoints = path;
    let validation = validate_route_with_zones(&state, &detour, &zones, 0.);
    assert!(validation.valid, "{:?}", validation.errors);
    assert!(validation.distance_m > blocked.distance_m);
    output_plan(
        &directory,
        "school-detour",
        &state,
        vec![direct.clone(), detour],
        &zones,
        None,
    );

    let w = &fixture["window"];
    let target = WindowTarget {
        window: point(&fixture, xyz(&w["position_enu"])),
        outward_bearing_deg: number(&w["outward_bearing_deg"]),
        standoff_m: number(&w["standoff_m"]),
        approach_distance_m: number(&w["approach_distance_m"]),
        hold_seconds: number(&w["hold_seconds"]),
    };
    validate_window(&target).unwrap();
    let mut delivery = direct;
    delivery.name = "dummy-school-window".into();
    delivery.waypoints = vec![start, target.window.clone()];
    let mut request = serde_json::to_value(&delivery).unwrap();
    request["window_target"] = serde_json::to_value(&target).unwrap();
    request["expires_in_seconds"] = json!(600);
    fs::write(
        directory.join("window-request.json"),
        serde_json::to_string_pretty(&request).unwrap(),
    )
    .unwrap();
    append_window(&mut delivery.waypoints, &target).unwrap();
    assert_eq!(delivery.waypoints.len(), 3);
    let hover = delivery.waypoints.last().unwrap();
    assert!(
        (crate::geo::haversine_m(
            (hover.lat, hover.lon),
            (target.window.lat, target.window.lon)
        ) - target.standoff_m)
            .abs()
            < 0.01
    );
    let validation = validate_route_with_zones(&state, &delivery, &zones, target.hold_seconds);
    assert!(validation.valid, "{:?}", validation.errors);
    let travel = validation.distance_m / delivery.cruise_speed_mps.unwrap();
    assert!((validation.estimated_duration_s - travel - target.hold_seconds).abs() < 1e-8);
    assert!(
        (validation.estimated_energy_wh
            - state.route_power_w * validation.estimated_duration_s / 3600.)
            .abs()
            < 1e-8
    );
    output_plan(
        &directory,
        "school-window",
        &state,
        vec![delivery],
        &zones,
        Some(target),
    );
}
