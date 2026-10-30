use super::*;

pub(super) fn constraints(state: &AppState, route: &RouteInput, hold: f64) -> Constraints {
    Constraints {
        max_altitude_m: route.max_altitude_m.unwrap_or(state.max_altitude_m),
        max_distance_m: state.route_max_distance_m,
        max_leg_m: state.route_max_leg_m,
        speed_mps: route.cruise_speed_mps.unwrap_or(8.0),
        power_w: state.route_power_w,
        battery_wh: state.route_battery_wh,
        hold_seconds: hold,
    }
}
pub(super) fn contains_required(points: &[Waypoint], required: &[Waypoint]) -> bool {
    let mut cursor = 0;
    for point in points {
        if required
            .get(cursor)
            .is_some_and(|required| same(point, required))
        {
            cursor += 1;
        }
    }
    cursor == required.len()
}

pub(super) fn build_payload(
    state: &AppState,
    body: PlanInput,
    zones: Vec<NoFlyZoneRow>,
) -> Result<PlanPayload, AppError> {
    if body.altitude_reference.is_some()
        && body.altitude_reference != state.automatic_plan_altitude_reference
    {
        return Err(bad(
            "plan altitude reference is not configured for this deployment",
        ));
    }
    let mut required = body.route.waypoints.clone();
    if let Some(window) = &body.window_target {
        append_window(&mut required, window)?;
    }
    let mut paths = vec![required.clone()];
    for mut points in body.alternatives {
        if let Some(window) = &body.window_target {
            append_window(&mut points, window)?;
        }
        if !contains_required(&points, &required)
            || !same(&points[0], &required[0])
            || !same(
                points.last().ok_or_else(|| bad("empty alternative"))?,
                required.last().ok_or_else(|| bad("empty route"))?,
            )
        {
            return Err(bad(
                "alternatives must preserve all required stops in order",
            ));
        }
        paths.push(points);
    }
    // With no obstacle vertices the visibility graph cannot add a detour.
    if !zones.is_empty() {
        let prepared_zones = PreparedZones::new(&zones)?;
        let mut detour = vec![required[0].clone()];
        for (index, leg) in required.windows(2).enumerate() {
            if body.window_target.is_some() && index + 2 == required.len() {
                detour.push(leg[1].clone());
                continue;
            }
            let path = prepared_zones.generate_detour(state, &leg[0], &leg[1], None);
            match path {
                Some(points) => detour.extend(points.into_iter().skip(1)),
                None => detour.push(leg[1].clone()),
            }
        }
        let exterior = body.window_target.as_ref().is_none_or(|w| {
            let b = w.outward_bearing_deg.to_radians();
            detour.iter().all(|p| {
                let north = (p.lat - w.window.lat).to_radians() * 6_371_000.0;
                let east = (p.lon - w.window.lon).to_radians()
                    * 6_371_000.0
                    * w.window.lat.to_radians().cos();
                north * b.cos() + east * b.sin() >= w.standoff_m - 0.05
            })
        });
        if detour.len() <= 256
            && exterior
            && (detour.len() != required.len()
                || detour.iter().zip(&required).any(|(a, b)| !same(a, b)))
        {
            paths.push(detour);
        }
    }
    let hold = body.window_target.as_ref().map_or(0.0, |w| w.hold_seconds);
    let mut candidates = Vec::new();
    for points in paths {
        if points.len() > 256 || !contains_required(&points, &required) {
            return Err(bad(
                "candidate exceeds waypoint limit or omits required stops",
            ));
        }
        if let Some(w) = &body.window_target {
            let b = w.outward_bearing_deg.to_radians();
            if points.iter().any(|p| {
                let north = (p.lat - w.window.lat).to_radians() * 6_371_000.0;
                let east = (p.lon - w.window.lon).to_radians()
                    * 6_371_000.0
                    * w.window.lat.to_radians().cos();
                north * b.cos() + east * b.sin() < w.standoff_m - 0.05
            }) {
                return Err(bad(
                    "window route must remain outside the facade standoff plane",
                ));
            }
        }
        let mut route = body.route.clone();
        route.waypoints = points;
        route.cruise_speed_mps = Some(route.cruise_speed_mps.unwrap_or(8.0));
        route.max_altitude_m = Some(route.max_altitude_m.unwrap_or(state.max_altitude_m));
        candidates.push(route);
    }
    let validations: Vec<_> = candidates
        .iter()
        .map(|r| validate_route_with_zones(state, r, &zones, hold))
        .collect();
    let selected_index = validations
        .iter()
        .enumerate()
        .filter(|(_, v)| v.valid)
        .min_by(|a, b| a.1.distance_m.total_cmp(&b.1.distance_m))
        .map(|(i, _)| i)
        .ok_or_else(|| {
            bad("no valid candidate route; provide alternatives avoiding active zones and limits")
        })?;
    Ok(PlanPayload { altitude_reference: body.altitude_reference, selected_index, constraints: constraints(state, &candidates[selected_index], hold), required_stops: required, candidates, validations, zones, window_target: body.window_target, optimality_scope: "minimum 3D distance among required-stop-preserving supplied and bounded visibility-graph candidates; no global optimality claim".into() })
}

pub(crate) fn automatic_payload(
    state: &AppState,
    route: RouteInput,
    zones: Vec<NoFlyZoneRow>,
) -> Result<PlanPayload, AppError> {
    build_payload(
        state,
        PlanInput {
            route,
            alternatives: Vec::new(),
            window_target: None,
            altitude_reference: state.automatic_plan_altitude_reference,
            expires_in_seconds: 1800,
        },
        zones,
    )
}
