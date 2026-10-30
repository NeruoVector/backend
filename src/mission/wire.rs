use super::*;

pub(super) fn fnv(text: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}
pub(super) fn plan_text(
    id: Uuid,
    drone: &str,
    expires: DateTime<Utc>,
    p: &PlanPayload,
) -> Result<String, AppError> {
    let c = &p.constraints;
    let mut s = format!(
        "NVPLAN2 {id} {drone} {} {} {}\n{:.17} {:.17} {:.17} {:.17} {:.17} {:.17} {:.17}\n",
        expires.timestamp(),
        p.selected_index,
        p.candidates.len(),
        c.max_altitude_m,
        c.max_distance_m,
        c.max_leg_m,
        c.speed_mps,
        c.power_w,
        c.battery_wh,
        c.hold_seconds
    );
    if let Some(w) = &p.window_target {
        s += &format!(
            "1 {:.17} {:.17} {:.17} {:.17} {:.17} {:.17} {:.17}\n",
            w.window.lat,
            w.window.lon,
            w.window.altitude_m,
            w.outward_bearing_deg,
            w.standoff_m,
            w.approach_distance_m,
            w.hold_seconds
        );
    } else {
        s += "0\n";
    }
    s += &format!("{}\n", p.required_stops.len());
    for w in &p.required_stops {
        s += &format!("{:.17} {:.17} {:.17}\n", w.lat, w.lon, w.altitude_m);
    }
    for candidate in &p.candidates {
        s += &format!("{}\n", candidate.waypoints.len());
        for w in &candidate.waypoints {
            s += &format!("{:.17} {:.17} {:.17}\n", w.lat, w.lon, w.altitude_m);
        }
    }
    s += &format!("{}\n", p.zones.len());
    for z in &p.zones {
        let points: Vec<Waypoint> =
            serde_json::from_value(z.polygon.clone()).map_err(|_| bad("invalid active zone"))?;
        s += &format!(
            "{:.17} {}\n",
            z.max_altitude_m.unwrap_or(-1.0),
            points.len()
        );
        for w in points {
            s += &format!("{:.17} {:.17}\n", w.lat, w.lon);
        }
    }
    Ok(s)
}
