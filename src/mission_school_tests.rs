use super::*;
use crate::state::test_state;
use serde_json::json;

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
        altitude_reference: None,
        constraints: constraints(state, &candidates[selected_index], hold),
        selected_index,
        required_stops: if window.is_some() {
            candidates[selected_index].waypoints.clone()
        } else {
            vec![
                candidates[0].waypoints[0].clone(),
                candidates[0].waypoints.last().unwrap().clone(),
            ]
        },
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
    let pool = crate::db::pool("postgres://dummy:dummy@127.0.0.1:1/dummy", 1).unwrap();
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

#[tokio::test]
#[ignore = "requires isolated TEST_DATABASE_URL with migrations"]
async fn round_trip_plan_preserves_customer_stop() {
    let url = std::env::var("TEST_DATABASE_URL").expect("isolated database URL");
    let state = test_state(crate::db::pool(&url, 2).unwrap());
    let home = Waypoint {
        lat: 37.499,
        lon: 127.0,
        altitude_m: 30.0,
    };
    let customer = Waypoint {
        lat: 37.5,
        lon: 127.0,
        altitude_m: 30.0,
    };
    let route = RouteInput {
        name: "regression-round-trip".into(),
        drone_id: Some("regression-drone".into()),
        waypoints: vec![home.clone(), customer.clone(), home],
        cruise_speed_mps: Some(5.0),
        max_altitude_m: Some(30.0),
    };
    let (_, Json(plan)) = create_plan(
        State(state),
        Json(PlanInput {
            altitude_reference: None,
            route,
            alternatives: vec![],
            window_target: None,
            expires_in_seconds: 600,
        }),
    )
    .await
    .unwrap();
    if let Ok(directory) = std::env::var("PLANNING_FIXTURE_DIR") {
        fs::write(
            PathBuf::from(directory).join("roundtrip.nvplan"),
            &plan.plan_text,
        )
        .unwrap();
    }
    let payload: PlanPayload = serde_json::from_value(plan.payload).unwrap();
    let selected = &payload.candidates[payload.selected_index];
    assert!(
        selected
            .waypoints
            .iter()
            .any(|point| same(point, &customer)),
        "selected plan omitted customer stop: {:?}",
        selected.waypoints
    );
    assert!((payload.validations[payload.selected_index].distance_m - 222.389853).abs() < 0.01);
}

#[tokio::test]
async fn candidates_keep_ordered_multistop_and_reject_shortcuts() {
    let pool = crate::db::pool("postgres://dummy:dummy@127.0.0.1:1/dummy", 1).unwrap();
    let state = test_state(pool);
    let home = Waypoint {
        lat: 37.499,
        lon: 127.0,
        altitude_m: 30.0,
    };
    let first = Waypoint {
        lat: 37.5,
        lon: 127.0,
        altitude_m: 30.0,
    };
    let second = Waypoint {
        lat: 37.501,
        lon: 127.0,
        altitude_m: 30.0,
    };
    let route = RouteInput {
        name: "multi".into(),
        drone_id: Some("test".into()),
        waypoints: vec![home.clone(), first.clone(), second.clone(), home.clone()],
        cruise_speed_mps: Some(5.0),
        max_altitude_m: Some(30.0),
    };
    let input = PlanInput {
        altitude_reference: None,
        route: route.clone(),
        alternatives: vec![],
        window_target: None,
        expires_in_seconds: 600,
    };
    let payload = build_payload(&state, input, vec![]).unwrap();
    if let Ok(directory) = std::env::var("PLANNING_FIXTURE_DIR") {
        let text = plan_text(
            Uuid::new_v4(),
            "test",
            Utc::now() + Duration::seconds(600),
            &payload,
        )
        .unwrap();
        fs::write(PathBuf::from(directory).join("multistop.nvplan"), text).unwrap();
    }
    assert!(
        payload
            .candidates
            .iter()
            .all(|candidate| contains_required(&candidate.waypoints, &route.waypoints))
    );
    let shortcut = PlanInput {
        altitude_reference: None,
        route,
        alternatives: vec![vec![home.clone(), second, first, home]],
        window_target: None,
        expires_in_seconds: 600,
    };
    assert!(build_payload(&state, shortcut, vec![]).is_err());
}

#[tokio::test]
async fn generated_detour_preserves_each_ordered_stop() {
    let pool = crate::db::pool("postgres://dummy:dummy@127.0.0.1:1/dummy", 1).unwrap();
    let state = test_state(pool);
    let waypoint = |lat, lon| Waypoint {
        lat,
        lon,
        altitude_m: 30.0,
    };
    let home = waypoint(37.499, 127.0);
    let customer = waypoint(37.501, 127.0);
    let polygon = vec![
        waypoint(37.4998, 126.9998),
        waypoint(37.5002, 126.9998),
        waypoint(37.5002, 127.0002),
        waypoint(37.4998, 127.0002),
    ];
    let zone = NoFlyZoneRow {
        id: Uuid::new_v4(),
        name: "test zone".into(),
        polygon: serde_json::to_value(polygon).unwrap(),
        max_altitude_m: Some(100.0),
    };
    let required = vec![home.clone(), customer, home];
    let route = RouteInput {
        name: "roundtrip detour".into(),
        drone_id: Some("test".into()),
        waypoints: required.clone(),
        cruise_speed_mps: Some(5.0),
        max_altitude_m: Some(30.0),
    };
    let payload = build_payload(
        &state,
        PlanInput {
            altitude_reference: None,
            route,
            alternatives: vec![],
            window_target: None,
            expires_in_seconds: 600,
        },
        vec![zone],
    )
    .unwrap();
    assert!(payload.candidates[payload.selected_index].waypoints.len() > required.len());
    assert!(
        payload
            .candidates
            .iter()
            .all(|candidate| contains_required(&candidate.waypoints, &required))
    );
}

#[tokio::test]
async fn planning_rejects_visibility_graph_limits_before_selecting_a_route() {
    let pool = crate::db::pool("postgres://dummy:dummy@127.0.0.1:1/dummy", 1).unwrap();
    let state = test_state(pool);
    let input = || PlanInput {
        altitude_reference: None,
        route: RouteInput {
            name: "bounded graph".into(),
            drone_id: Some("test".into()),
            waypoints: vec![
                Waypoint {
                    lat: 37.0,
                    lon: 127.0,
                    altitude_m: 30.0,
                },
                Waypoint {
                    lat: 37.001,
                    lon: 127.0,
                    altitude_m: 30.0,
                },
            ],
            cruise_speed_mps: Some(5.0),
            max_altitude_m: Some(30.0),
        },
        alternatives: vec![],
        window_target: None,
        expires_in_seconds: 600,
    };
    let zone = NoFlyZoneRow {
        id: Uuid::new_v4(),
        name: "distant zone".into(),
        polygon: json!([
            {"lat":38.0,"lon":128.0,"altitude_m":30.0},
            {"lat":38.0,"lon":128.001,"altitude_m":30.0},
            {"lat":38.001,"lon":128.001,"altitude_m":30.0},
            {"lat":38.001,"lon":128.0,"altitude_m":30.0}
        ]),
        max_altitude_m: Some(100.0),
    };

    assert!(build_payload(&state, input(), vec![zone.clone(); 31]).is_ok());
    assert!(build_payload(&state, input(), vec![zone.clone(); 32]).is_err());
    assert!(build_payload(&state, input(), vec![zone; 65]).is_err());
}

#[tokio::test]
async fn window_required_stops_replace_facade_with_exterior_points() {
    let pool = crate::db::pool("postgres://dummy:dummy@127.0.0.1:1/dummy", 1).unwrap();
    let state = test_state(pool);
    let point = |lat| Waypoint {
        lat,
        lon: 127.0,
        altitude_m: 30.0,
    };
    let start = point(37.498);
    let intermediate = point(37.499);
    let window = point(37.5);
    let route = RouteInput {
        name: "window".into(),
        drone_id: Some("test".into()),
        waypoints: vec![start, intermediate.clone(), window.clone()],
        cruise_speed_mps: Some(5.0),
        max_altitude_m: Some(30.0),
    };
    let target = WindowTarget {
        window: window.clone(),
        outward_bearing_deg: 180.0,
        standoff_m: 3.0,
        approach_distance_m: 15.0,
        hold_seconds: 30.0,
    };
    let payload = build_payload(
        &state,
        PlanInput {
            altitude_reference: None,
            route,
            alternatives: vec![],
            window_target: Some(target),
            expires_in_seconds: 600,
        },
        vec![],
    )
    .unwrap();
    assert!(same(&payload.required_stops[1], &intermediate));
    assert!(!payload.required_stops.iter().any(|p| same(p, &window)));
    assert!(
        payload
            .candidates
            .iter()
            .all(|candidate| contains_required(&candidate.waypoints, &payload.required_stops))
    );
    assert_eq!(payload.constraints.hold_seconds, 30.0);
    assert!(delivery_stops_match(
        &payload,
        &[(Some(window.lat), Some(window.lon))],
        false
    ));
    assert!(!delivery_stops_match(
        &payload,
        &[(Some(window.lat), Some(window.lon))],
        true
    ));
}

#[tokio::test]
async fn dispatch_stop_guard_accepts_one_way_and_rejects_missing_roundtrip_customer() {
    let pool = crate::db::pool("postgres://dummy:dummy@127.0.0.1:1/dummy", 1).unwrap();
    let state = test_state(pool);
    let home = Waypoint {
        lat: 37.499,
        lon: 127.0,
        altitude_m: 30.0,
    };
    let customer = Waypoint {
        lat: 37.5,
        lon: 127.0,
        altitude_m: 30.0,
    };
    let route = RouteInput {
        name: "oneway".into(),
        drone_id: Some("test".into()),
        waypoints: vec![home, customer.clone()],
        cruise_speed_mps: Some(5.0),
        max_altitude_m: Some(30.0),
    };
    let payload = build_payload(
        &state,
        PlanInput {
            altitude_reference: None,
            route,
            alternatives: vec![],
            window_target: None,
            expires_in_seconds: 600,
        },
        vec![],
    )
    .unwrap();
    let stops = [(Some(customer.lat), Some(customer.lon))];
    assert!(delivery_stops_match(&payload, &stops, false));
    assert!(!delivery_stops_match(&payload, &stops, true));
    assert!(!delivery_stops_match(&payload, &[], true));
}
