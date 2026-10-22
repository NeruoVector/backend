//! Bounded candidate planning and independent drone cross-check before dispatch.
use crate::{
    error::AppError,
    geo::Waypoint,
    routes::route_plans::{
        NoFlyZoneRow, RouteInput, RouteValidation, fetch_route, load_active_zones,
        validate_lat_lon, validate_route_with_zones,
    },
    state::AppState,
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct WindowTarget {
    pub window: Waypoint,
    pub outward_bearing_deg: f64,
    pub standoff_m: f64,
    pub approach_distance_m: f64,
    pub hold_seconds: f64,
}
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct PlanInput {
    #[serde(flatten)]
    pub route: RouteInput,
    #[serde(default)]
    pub alternatives: Vec<Vec<Waypoint>>,
    pub window_target: Option<WindowTarget>,
    #[serde(default = "default_ttl")]
    pub expires_in_seconds: i64,
}
fn default_ttl() -> i64 {
    600
}
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Constraints {
    pub max_altitude_m: f64,
    pub max_distance_m: f64,
    pub max_leg_m: f64,
    pub speed_mps: f64,
    pub power_w: f64,
    pub battery_wh: f64,
    pub hold_seconds: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PlanPayload {
    pub selected_index: usize,
    pub candidates: Vec<RouteInput>,
    pub validations: Vec<RouteValidation>,
    pub constraints: Constraints,
    pub zones: Vec<NoFlyZoneRow>,
    pub window_target: Option<WindowTarget>,
    pub optimality_scope: String,
}
#[derive(Debug, Serialize, utoipa::ToSchema, sqlx::FromRow)]
pub struct MissionPlan {
    pub id: Uuid,
    pub route_id: Uuid,
    pub drone_id: String,
    pub status: String,
    pub route_fingerprint: String,
    pub plan_text: String,
    pub payload: Value,
    pub cross_check: Option<Value>,
    pub expires_at: DateTime<Utc>,
    pub verified_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}
#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CrossCheck {
    pub drone_id: String,
    pub route_fingerprint: String,
    pub valid: bool,
    pub selected_index: usize,
    pub distance_m: f64,
    pub estimated_duration_s: f64,
    pub estimated_energy_wh: f64,
}
fn bad(s: impl Into<String>) -> AppError {
    AppError::BadRequest(s.into())
}
fn offset(w: &Waypoint, bearing: f64, distance: f64) -> Waypoint {
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
fn same(a: &Waypoint, b: &Waypoint) -> bool {
    (a.lat - b.lat).abs() < 1e-10
        && (a.lon - b.lon).abs() < 1e-10
        && (a.altitude_m - b.altitude_m).abs() < 1e-6
}
fn validate_window(w: &WindowTarget) -> Result<(), AppError> {
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
fn append_window(points: &mut Vec<Waypoint>, w: &WindowTarget) -> Result<(), AppError> {
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
fn fnv(text: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}
fn plan_text(
    id: Uuid,
    drone: &str,
    expires: DateTime<Utc>,
    p: &PlanPayload,
) -> Result<String, AppError> {
    let c = &p.constraints;
    let mut s = format!(
        "NVPLAN1 {id} {drone} {} {} {}\n{:.17} {:.17} {:.17} {:.17} {:.17} {:.17} {:.17}\n",
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
fn constraints(state: &AppState, route: &RouteInput, hold: f64) -> Constraints {
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
/// Bounded local visibility graph. Offset vertices are candidate clearance points,
/// and every graph edge is checked against every complete restricted polygon.
fn generate_detour(
    state: &AppState,
    start: &Waypoint,
    end: &Waypoint,
    zones: &[NoFlyZoneRow],
    window: Option<&WindowTarget>,
) -> Result<Option<Vec<Waypoint>>, AppError> {
    if zones.len() > 64 {
        return Err(bad("planning supports at most 64 active no-fly zones"));
    }
    let mut polygons = Vec::new();
    let mut vertices = 0;
    for z in zones {
        let p: Vec<Waypoint> = serde_json::from_value(z.polygon.clone())
            .map_err(|_| bad("invalid active no-fly polygon"))?;
        if !(3..=256).contains(&p.len())
            || p.iter().any(|w| validate_lat_lon(w.lat, w.lon).is_err())
        {
            return Err(bad("invalid active no-fly polygon"));
        }
        vertices += p.len();
        polygons.push((p, z.max_altitude_m));
    }
    if vertices > 126 {
        return Err(bad(
            "visibility graph supports at most 126 active polygon vertices",
        ));
    }
    let goal = window.map_or_else(
        || end.clone(),
        |w| offset(&w.window, w.outward_bearing_deg, w.approach_distance_m),
    );
    let exterior = |p: &Waypoint| -> bool {
        window.is_none_or(|w| {
            let b = w.outward_bearing_deg.to_radians();
            let n = (p.lat - w.window.lat).to_radians() * 6_371_000.0;
            let e =
                (p.lon - w.window.lon).to_radians() * 6_371_000.0 * w.window.lat.to_radians().cos();
            n * b.cos() + e * b.sin() >= w.standoff_m - 0.05
        })
    };
    let clear = |a: &Waypoint, b: &Waypoint| -> bool {
        (a.lon - b.lon).abs() <= 180.0
            && exterior(a)
            && exterior(b)
            && crate::geo::haversine_m((a.lat, a.lon), (b.lat, b.lon))
                .hypot(b.altitude_m - a.altitude_m)
                <= state.route_max_leg_m
            && polygons
                .iter()
                .all(|(p, c)| !crate::routes::route_plans::segment_hits_zone(a, b, p, *c))
    };
    if clear(start, &goal) {
        return Ok(None);
    }
    let mut nodes = vec![start.clone(), goal.clone()];
    for (polygon, _) in &polygons {
        let center_lat = polygon.iter().map(|w| w.lat).sum::<f64>() / polygon.len() as f64;
        let center_lon = polygon.iter().map(|w| w.lon).sum::<f64>() / polygon.len() as f64;
        for vertex in polygon {
            let bearing = ((vertex.lon - center_lon) * center_lat.to_radians().cos())
                .atan2(vertex.lat - center_lat)
                .to_degrees();
            let mut p = offset(vertex, bearing, 8.0);
            p.altitude_m = goal.altitude_m;
            if exterior(&p) {
                nodes.push(p);
            }
        }
    }
    let n = nodes.len();
    let mut distance = vec![f64::INFINITY; n];
    let mut previous = vec![None; n];
    let mut visited = vec![false; n];
    distance[0] = 0.0;
    for _ in 0..n {
        let Some(u) = (0..n)
            .filter(|i| !visited[*i])
            .min_by(|a, b| distance[*a].total_cmp(&distance[*b]))
        else {
            break;
        };
        if !distance[u].is_finite() || u == 1 {
            break;
        }
        visited[u] = true;
        for v in 0..n {
            if visited[v] || !clear(&nodes[u], &nodes[v]) {
                continue;
            }
            let next = distance[u]
                + crate::geo::haversine_m(
                    (nodes[u].lat, nodes[u].lon),
                    (nodes[v].lat, nodes[v].lon),
                )
                .hypot(nodes[v].altitude_m - nodes[u].altitude_m);
            if next < distance[v] {
                distance[v] = next;
                previous[v] = Some(u);
            }
        }
    }
    if !distance[1].is_finite() {
        return Ok(None);
    }
    let mut indices = vec![1];
    let mut cursor = 1;
    while cursor != 0 {
        let Some(parent) = previous[cursor] else {
            return Ok(None);
        };
        indices.push(parent);
        cursor = parent;
    }
    indices.reverse();
    let mut path: Vec<_> = indices.into_iter().map(|i| nodes[i].clone()).collect();
    // Window assembly appends the exact outside approach and hover. Remove graph goal first.
    if window.is_some() {
        path.pop();
        path.push(end.clone());
    }
    Ok(Some(path))
}

#[utoipa::path(post,path="/api/v1/routes/plan",tag="routes",request_body=PlanInput,responses((status=201,description="Bounded candidate plan pending drone cross-check",body=MissionPlan)))]
pub async fn create_plan(
    State(state): State<AppState>,
    Json(body): Json<PlanInput>,
) -> Result<(StatusCode, Json<MissionPlan>), AppError> {
    let drone = body
        .route
        .drone_id
        .as_deref()
        .ok_or_else(|| bad("drone_id is required for a mission"))?;
    if drone.is_empty()
        || drone.len() > 128
        || !drone
            .bytes()
            .all(|v| v.is_ascii_alphanumeric() || b"-_.".contains(&v))
    {
        return Err(bad("drone_id must be a 1..128 character identifier"));
    }
    if !(30..=1800).contains(&body.expires_in_seconds)
        || !(2..=254).contains(&body.route.waypoints.len())
        || body.alternatives.len() > 13
        || body
            .alternatives
            .iter()
            .any(|v| !(2..=254).contains(&v.len()))
    {
        return Err(bad("plan expiry/candidate/waypoint limits exceeded"));
    }
    if let Some(w) = &body.window_target {
        validate_window(w)?;
    }
    let start = &body.route.waypoints[0];
    let end = body.route.waypoints.last().unwrap();
    if let Some(w) = &body.window_target
        && !same(end, &w.window)
    {
        return Err(bad(
            "window route final input waypoint must match window_target.window",
        ));
    }
    for w in body
        .route
        .waypoints
        .iter()
        .chain(body.alternatives.iter().flatten())
    {
        validate_lat_lon(w.lat, w.lon).map_err(bad)?;
        if !w.altitude_m.is_finite() || w.altitude_m <= 0.0 {
            return Err(bad("waypoint altitude must be finite and positive"));
        }
    }
    let zones = load_active_zones(&state.pool).await?;
    let mut paths = vec![vec![start.clone(), end.clone()]];
    if body.route.waypoints.len() > 2 {
        paths.push(body.route.waypoints.clone());
    }
    for points in &body.alternatives {
        if !same(&points[0], start) || !same(points.last().unwrap(), end) {
            return Err(bad(
                "alternatives must share departure and destination including altitude",
            ));
        }
        paths.push(points.clone());
    }
    if let Some(detour) = generate_detour(&state, start, end, &zones, body.window_target.as_ref())?
    {
        paths.push(detour);
    }
    let hold = body.window_target.as_ref().map_or(0.0, |w| w.hold_seconds);
    let mut candidates = Vec::new();
    for mut points in paths {
        if let Some(w) = &body.window_target {
            append_window(&mut points, w)?;
        }
        let mut r = body.route.clone();
        r.waypoints = points;
        r.cruise_speed_mps = Some(r.cruise_speed_mps.unwrap_or(8.0));
        r.max_altitude_m = Some(r.max_altitude_m.unwrap_or(state.max_altitude_m));
        candidates.push(r);
    }
    let validations: Vec<_> = candidates
        .iter()
        .map(|r| validate_route_with_zones(&state, r, &zones, hold))
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
    let payload=PlanPayload{selected_index,constraints:constraints(&state,&candidates[selected_index],hold),candidates,validations,zones,window_target:body.window_target,optimality_scope:"minimum 3D distance among direct, supplied and bounded visibility-graph detour candidates; no global optimality claim".into()};
    let id = Uuid::new_v4();
    let expires = Utc::now() + Duration::seconds(body.expires_in_seconds);
    let text = plan_text(id, drone, expires, &payload)?;
    let fingerprint = fnv(&text);
    let route = &payload.candidates[selected_index];
    let v = &payload.validations[selected_index];
    let mut tx = state.pool.begin().await?;
    let route_id:Uuid=sqlx::query_scalar("INSERT INTO neurovector.routes(name,drone_id,waypoints,cruise_speed_mps,max_altitude_m,distance_m,estimated_duration_s) VALUES($1,$2,$3,$4,$5,$6,$7) RETURNING id")
        .bind(&route.name).bind(drone).bind(serde_json::to_value(&route.waypoints).map_err(|e|bad(e.to_string()))?).bind(route.cruise_speed_mps.unwrap()).bind(route.max_altitude_m.unwrap()).bind(v.distance_m).bind(v.estimated_duration_s).fetch_one(&mut *tx).await?;
    let plan=sqlx::query_as::<_,MissionPlan>("INSERT INTO neurovector.mission_plans(id,route_id,drone_id,route_fingerprint,plan_text,payload,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7) RETURNING *")
        .bind(id).bind(route_id).bind(drone).bind(fingerprint).bind(text).bind(serde_json::to_value(payload).map_err(|e|bad(e.to_string()))?).bind(expires).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(plan)))
}
#[utoipa::path(get,path="/api/v1/routes/plans/{id}",tag="routes",params(("id"=Uuid,Path,description="Mission plan ID")),responses((status=200,description="Persisted mission and drone protocol",body=MissionPlan)))]
pub async fn get_plan(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<MissionPlan>, AppError> {
    Ok(Json(fetch_plan(&state, id).await?))
}
async fn fetch_plan(state: &AppState, id: Uuid) -> Result<MissionPlan, AppError> {
    sqlx::query_as::<_, MissionPlan>("SELECT * FROM neurovector.mission_plans WHERE id=$1")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(AppError::NotFound)
}

async fn revalidate(
    state: &AppState,
    plan: &MissionPlan,
    drone: &str,
) -> Result<(PlanPayload, RouteValidation), AppError> {
    if plan.expires_at <= Utc::now() || plan.drone_id != drone {
        return Err(bad("mission expired or assigned to another drone"));
    }
    if fnv(&plan.plan_text) != plan.route_fingerprint {
        return Err(bad("stored mission fingerprint mismatch"));
    }
    let p: PlanPayload =
        serde_json::from_value(plan.payload.clone()).map_err(|e| bad(e.to_string()))?;
    if plan_text(plan.id, &plan.drone_id, plan.expires_at, &p)? != plan.plan_text {
        return Err(bad("stored mission payload changed"));
    }
    let r = p
        .candidates
        .get(p.selected_index)
        .ok_or_else(|| bad("invalid selected candidate"))?;
    let zones = load_active_zones(&state.pool).await?;
    if serde_json::to_value(&zones).ok() != serde_json::to_value(&p.zones).ok() {
        return Err(bad("no-fly zones changed; create a fresh mission plan"));
    }
    let current = constraints(state, r, p.constraints.hold_seconds);
    if serde_json::to_value(current).ok() != serde_json::to_value(&p.constraints).ok() {
        return Err(bad(
            "flight constraints changed; create a fresh mission plan",
        ));
    }
    let validations: Vec<_> = p
        .candidates
        .iter()
        .map(|r| validate_route_with_zones(state, r, &zones, p.constraints.hold_seconds))
        .collect();
    let best = validations
        .iter()
        .enumerate()
        .filter(|(_, v)| v.valid)
        .min_by(|a, b| a.1.distance_m.total_cmp(&b.1.distance_m))
        .map(|(i, _)| i);
    if best != Some(p.selected_index) {
        return Err(bad(
            "selected candidate is no longer the minimum valid route",
        ));
    }
    let stored = fetch_route(&state.pool, plan.route_id).await?;
    if stored.drone_id.as_deref() != Some(drone)
        || stored.waypoints != serde_json::to_value(&r.waypoints).map_err(|e| bad(e.to_string()))?
        || Some(stored.cruise_speed_mps) != r.cruise_speed_mps
        || Some(stored.max_altitude_m) != r.max_altitude_m
    {
        return Err(bad("saved route no longer matches the verified mission"));
    }
    let v = validations[p.selected_index].clone();
    Ok((p, v))
}
fn close(a: f64, b: f64) -> bool {
    a.is_finite() && b.is_finite() && (a - b).abs() <= 0.01_f64.max(b.abs() * 1e-6)
}
#[utoipa::path(post,path="/api/v1/routes/plans/{id}/cross-check",tag="routes",params(("id"=Uuid,Path,description="Mission plan ID")),request_body=CrossCheck,responses((status=200,description="Drone and backend cross-check agree",body=MissionPlan)))]
pub async fn cross_check(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(report): Json<CrossCheck>,
) -> Result<Json<MissionPlan>, AppError> {
    let plan = fetch_plan(&state, id).await?;
    if report.drone_id != plan.drone_id {
        return Err(AppError::Conflict(
            "mission assigned to another drone".into(),
        ));
    }
    let checked = revalidate(&state, &plan, &report.drone_id).await;
    let valid = match &checked {
        Ok((p, v)) => {
            report.valid
                && report.route_fingerprint == plan.route_fingerprint
                && report.selected_index == p.selected_index
                && close(report.distance_m, v.distance_m)
                && close(report.estimated_duration_s, v.estimated_duration_s)
                && close(report.estimated_energy_wh, v.estimated_energy_wh)
        }
        Err(_) => false,
    };
    let value = serde_json::to_value(&report).map_err(|e| bad(e.to_string()))?;
    let updated=sqlx::query("UPDATE neurovector.mission_plans SET status=$2,cross_check=$3,verified_at=CASE WHEN $2='verified' THEN now() ELSE NULL END WHERE id=$1 AND expires_at>now()")
        .bind(id).bind(if valid{"verified"}else{"rejected"}).bind(value).execute(&state.pool).await?;
    if updated.rows_affected() != 1 {
        return Err(AppError::Conflict(
            "mission expired during cross-check".into(),
        ));
    }
    checked?;
    if !valid {
        return Err(bad(
            "drone cross-check disagrees with route identity, optimality, validity or metrics",
        ));
    }
    Ok(Json(fetch_plan(&state, id).await?))
}
pub async fn verify_route_for_dispatch(
    state: &AppState,
    route_id: Uuid,
    drone_id: &str,
) -> Result<(), AppError> {
    let plan = sqlx::query_as::<_, MissionPlan>(
        "SELECT * FROM neurovector.mission_plans WHERE route_id=$1",
    )
    .bind(route_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| {
        AppError::Conflict("create a cross-validated mission plan before dispatch".into())
    })?;
    if plan.status != "verified" || plan.verified_at.is_none() {
        return Err(bad("mission has not passed drone cross-check"));
    }
    revalidate(state, &plan, drone_id).await?;
    Ok(())
}

#[cfg(test)]
#[path = "mission_school_tests.rs"]
mod school_tests;
