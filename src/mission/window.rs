use super::*;

pub(super) fn offset(w: &Waypoint, bearing: f64, distance: f64) -> Waypoint {
    let angle = distance / 6_371_000.0;
    let b = bearing.to_radians();
    let lat = w.lat.to_radians();
    let lon = w.lon.to_radians();
    let out_lat = (lat.sin() * angle.cos() + lat.cos() * angle.sin() * b.cos()).asin();
    let out_lon =
        lon + (b.sin() * angle.sin() * lat.cos()).atan2(angle.cos() - lat.sin() * out_lat.sin());
    Waypoint {
        lat: out_lat.to_degrees(),
        lon: (out_lon.to_degrees() + 540.0) % 360.0 - 180.0,
        altitude_m: w.altitude_m,
    }
}
pub(super) fn same(a: &Waypoint, b: &Waypoint) -> bool {
    (a.lat - b.lat).abs() < 1e-10
        && (a.lon - b.lon).abs() < 1e-10
        && (a.altitude_m - b.altitude_m).abs() < 1e-6
}
pub(super) fn validate_window(w: &WindowTarget) -> Result<(), AppError> {
    validate_lat_lon(w.window.lat, w.window.lon).map_err(bad)?;
    if ![
        w.window.altitude_m,
        w.outward_bearing_deg,
        w.standoff_m,
        w.approach_distance_m,
        w.hold_seconds,
    ]
    .iter()
    .all(|v| v.is_finite())
        || w.window.altitude_m <= 0.0
        || !(0.0..360.0).contains(&w.outward_bearing_deg)
        || w.standoff_m < 2.0
        || w.approach_distance_m <= w.standoff_m
        || w.approach_distance_m > 1000.0
        || !(1.0..=3600.0).contains(&w.hold_seconds)
    {
        return Err(bad(
            "window requires finite altitude/bearing, standoff >= 2m, approach > standoff and <= 1000m, hold 1..3600s",
        ));
    }
    Ok(())
}
pub(super) fn append_window(points: &mut Vec<Waypoint>, w: &WindowTarget) -> Result<(), AppError> {
    if points.last().is_some_and(|last| same(last, &w.window)) {
        points.pop();
    }
    if points.is_empty() {
        return Err(bad("window route requires a departure waypoint"));
    }
    points.push(offset(
        &w.window,
        w.outward_bearing_deg,
        w.approach_distance_m,
    ));
    points.push(offset(&w.window, w.outward_bearing_deg, w.standoff_m));
    // Do not accept a transit segment on the building/interior side of the window plane.
    let b = w.outward_bearing_deg.to_radians();
    let lat_scale = w.window.lat.to_radians().cos();
    if points.iter().any(|p| {
        let north = (p.lat - w.window.lat).to_radians() * 6_371_000.0;
        let east = (p.lon - w.window.lon).to_radians() * 6_371_000.0 * lat_scale;
        north * b.cos() + east * b.sin() < w.standoff_m - 0.05
    }) {
        return Err(bad(
            "window route must remain outside the facade standoff plane",
        ));
    }
    Ok(())
}
