use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{delete, get, post},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    error::AppError,
    geo::{self, Waypoint},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_routes).post(create_route))
        .route("/validate", post(validate_route_endpoint))
        .route("/plan", post(crate::mission::create_plan))
        .route("/plans/{id}", get(crate::mission::get_plan))
        .route("/plans/{id}/cross-check", post(crate::mission::cross_check))
        .route("/{id}", get(get_route).delete(delete_route))
        .route("/{id}/validation", get(validate_saved_route))
        .route(
            "/no-fly-zones",
            get(list_no_fly_zones).post(create_no_fly_zone),
        )
        .route("/no-fly-zones/{id}", delete(delete_no_fly_zone))
}

#[derive(Debug, Serialize, utoipa::ToSchema, sqlx::FromRow)]
pub struct Route {
    pub id: Uuid,
    pub name: String,
    pub drone_id: Option<String>,
    pub waypoints: Value,
    pub cruise_speed_mps: f64,
    pub max_altitude_m: f64,
    pub distance_m: f64,
    pub estimated_duration_s: f64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RouteInput {
    pub name: String,
    #[serde(default)]
    pub drone_id: Option<String>,
    pub waypoints: Vec<Waypoint>,
    #[serde(default)]
    pub cruise_speed_mps: Option<f64>,
    #[serde(default)]
    pub max_altitude_m: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RouteValidation {
    pub valid: bool,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub waypoint_count: usize,
    pub distance_m: f64,
    pub estimated_duration_s: f64,
    pub estimated_energy_wh: f64,
    pub max_altitude_m: f64,
    pub leg_lengths_m: Vec<f64>,
    pub no_fly_zone_conflicts: Vec<ZoneConflict>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ZoneConflict {
    pub zone_id: Uuid,
    pub zone_name: String,
    pub reason: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema, sqlx::FromRow)]
pub struct NoFlyZone {
    pub id: Uuid,
    pub name: String,
    pub polygon: Value,
    pub max_altitude_m: Option<f64>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateNoFlyZone {
    pub name: String,
    pub polygon: Vec<Waypoint>,
    #[serde(default)]
    pub max_altitude_m: Option<f64>,
    #[serde(default = "default_true")]
    pub active: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct ListRoutesParams {
    #[serde(default)]
    pub drone_id: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema, sqlx::FromRow)]
pub struct NoFlyZoneRow {
    pub id: Uuid,
    pub name: String,
    pub polygon: Value,
    pub max_altitude_m: Option<f64>,
}

async fn list_routes(
    State(state): State<AppState>,
    Query(params): Query<ListRoutesParams>,
) -> Result<Json<Vec<Route>>, AppError> {
    let limit = params.limit.unwrap_or(50).clamp(1, 200);
    let routes = sqlx::query_as::<_, Route>(
        r#"
        SELECT id, name, drone_id, waypoints, cruise_speed_mps, max_altitude_m,
               distance_m, estimated_duration_s, created_at, updated_at
        FROM neurovector.routes
        WHERE ($1::text IS NULL OR drone_id = $1)
        ORDER BY created_at DESC
        LIMIT $2
        "#,
    )
    .bind(params.drone_id.as_deref())
    .bind(limit)
    .fetch_all(&state.pool)
    .await?;

    Ok(Json(routes))
}

async fn get_route(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Route>, AppError> {
    let route = fetch_route(&state.pool, id).await?;
    Ok(Json(route))
}

async fn create_route(
    State(state): State<AppState>,
    Json(body): Json<RouteInput>,
) -> Result<(StatusCode, Json<Route>), AppError> {
    let validation = validate_route(&state, &body).await?;
    if !validation.valid {
        return Err(AppError::BadRequest(format!(
            "route validation failed: {}",
            validation.errors.join("; ")
        )));
    }

    let cruise_speed_mps = body.cruise_speed_mps.unwrap_or(8.0);
    let max_altitude_m = body.max_altitude_m.unwrap_or(state.max_altitude_m);
    let waypoints = serde_json::to_value(&body.waypoints)
        .map_err(|err| AppError::BadRequest(format!("invalid waypoints: {err}")))?;

    let route = sqlx::query_as::<_, Route>(
        r#"
        INSERT INTO neurovector.routes
            (name, drone_id, waypoints, cruise_speed_mps, max_altitude_m,
             distance_m, estimated_duration_s)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        RETURNING id, name, drone_id, waypoints, cruise_speed_mps, max_altitude_m,
                  distance_m, estimated_duration_s, created_at, updated_at
        "#,
    )
    .bind(&body.name)
    .bind(body.drone_id.as_deref())
    .bind(waypoints)
    .bind(cruise_speed_mps)
    .bind(max_altitude_m)
    .bind(validation.distance_m)
    .bind(validation.estimated_duration_s)
    .fetch_one(&state.pool)
    .await?;

    Ok((StatusCode::CREATED, Json(route)))
}

async fn delete_route(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let result = sqlx::query("DELETE FROM neurovector.routes WHERE id = $1")
        .bind(id)
        .execute(&state.pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn validate_route_endpoint(
    State(state): State<AppState>,
    Json(body): Json<RouteInput>,
) -> Result<Json<RouteValidation>, AppError> {
    Ok(Json(validate_route(&state, &body).await?))
}

async fn validate_saved_route(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<RouteValidation>, AppError> {
    let route = fetch_route(&state.pool, id).await?;
    let waypoints = serde_json::from_value::<Vec<Waypoint>>(route.waypoints)
        .map_err(|err| AppError::Other(anyhow::anyhow!("stored route is invalid: {err}")))?;

    let input = RouteInput {
        name: route.name,
        drone_id: route.drone_id,
        waypoints,
        cruise_speed_mps: Some(route.cruise_speed_mps),
        max_altitude_m: Some(route.max_altitude_m),
    };
    Ok(Json(validate_route(&state, &input).await?))
}

async fn list_no_fly_zones(
    State(state): State<AppState>,
) -> Result<Json<Vec<NoFlyZone>>, AppError> {
    let zones = sqlx::query_as::<_, NoFlyZone>(
        r#"
        SELECT id, name, polygon, max_altitude_m, active, created_at
        FROM neurovector.no_fly_zones
        ORDER BY created_at DESC
        "#,
    )
    .fetch_all(&state.pool)
    .await?;

    Ok(Json(zones))
}

async fn create_no_fly_zone(
    State(state): State<AppState>,
    Json(body): Json<CreateNoFlyZone>,
) -> Result<(StatusCode, Json<NoFlyZone>), AppError> {
    if body.name.trim().is_empty() {
        return Err(AppError::BadRequest("name is required".into()));
    }
    if !(3..=256).contains(&body.polygon.len()) {
        return Err(AppError::BadRequest(
            "polygon must have 3 to 256 points".into(),
        ));
    }
    for point in &body.polygon {
        validate_lat_lon(point.lat, point.lon).map_err(AppError::BadRequest)?;
    }
    if let Some(max_altitude_m) = body.max_altitude_m
        && (!max_altitude_m.is_finite() || max_altitude_m <= 0.0)
    {
        return Err(AppError::BadRequest(
            "max_altitude_m must be positive".into(),
        ));
    }

    let polygon = serde_json::to_value(&body.polygon)
        .map_err(|err| AppError::BadRequest(format!("invalid polygon: {err}")))?;

    let zone = sqlx::query_as::<_, NoFlyZone>(
        r#"
        INSERT INTO neurovector.no_fly_zones (name, polygon, max_altitude_m, active)
        VALUES ($1, $2, $3, $4)
        RETURNING id, name, polygon, max_altitude_m, active, created_at
        "#,
    )
    .bind(&body.name)
    .bind(polygon)
    .bind(body.max_altitude_m)
    .bind(body.active)
    .fetch_one(&state.pool)
    .await?;

    Ok((StatusCode::CREATED, Json(zone)))
}

async fn delete_no_fly_zone(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let result = sqlx::query("DELETE FROM neurovector.no_fly_zones WHERE id = $1")
        .bind(id)
        .execute(&state.pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn load_active_zones(pool: &PgPool) -> Result<Vec<NoFlyZoneRow>, AppError> {
    Ok(sqlx::query_as::<_, NoFlyZoneRow>(
        "SELECT id, name, polygon, max_altitude_m FROM neurovector.no_fly_zones WHERE active = true ORDER BY id",
    ).fetch_all(pool).await?)
}

pub async fn validate_route(
    state: &AppState,
    input: &RouteInput,
) -> Result<RouteValidation, AppError> {
    let zones = load_active_zones(&state.pool).await?;
    Ok(validate_route_with_zones(state, input, &zones, 0.0))
}

pub fn validate_route_with_zones(
    state: &AppState,
    input: &RouteInput,
    zones: &[NoFlyZoneRow],
    hold_seconds: f64,
) -> RouteValidation {
    let mut result = RouteValidation {
        valid: false,
        errors: Vec::new(),
        warnings: Vec::new(),
        waypoint_count: input.waypoints.len(),
        distance_m: 0.0,
        estimated_duration_s: 0.0,
        estimated_energy_wh: 0.0,
        max_altitude_m: input.max_altitude_m.unwrap_or(state.max_altitude_m),
        leg_lengths_m: Vec::new(),
        no_fly_zone_conflicts: Vec::new(),
    };
    let speed = input.cruise_speed_mps.unwrap_or(8.0);
    if input.name.trim().is_empty() {
        result.errors.push("name is required".into());
    }
    if !(2..=256).contains(&input.waypoints.len()) {
        result
            .errors
            .push("route must have 2 to 256 waypoints".into());
    }
    if !speed.is_finite() || speed <= 0.0 || speed > 100.0 {
        result
            .errors
            .push("cruise_speed_mps must be finite and in (0, 100]".into());
    }
    if !result.max_altitude_m.is_finite()
        || result.max_altitude_m <= 0.0
        || result.max_altitude_m > state.max_altitude_m
    {
        result
            .errors
            .push("max_altitude_m exceeds configured altitude limit".into());
    }
    if !hold_seconds.is_finite() || !(0.0..=3600.0).contains(&hold_seconds) {
        result
            .errors
            .push("hold_seconds must be finite and in [0, 3600]".into());
    }
    for (i, w) in input.waypoints.iter().enumerate() {
        if let Err(e) = validate_lat_lon(w.lat, w.lon) {
            result.errors.push(format!("waypoint {i}: {e}"));
        }
        if !w.altitude_m.is_finite() || w.altitude_m <= 0.0 || w.altitude_m > result.max_altitude_m
        {
            result
                .errors
                .push(format!("waypoint {i}: invalid or excessive altitude"));
        }
    }
    // Reject malformed/oversized inputs before geometry or allocation proportional to route length.
    if !result.errors.is_empty() {
        return result;
    }
    for pair in input.waypoints.windows(2) {
        if (pair[0].lon - pair[1].lon).abs() > 180.0 {
            result
                .errors
                .push("antimeridian-crossing legs are unsupported".into());
        }
        let leg = geo::haversine_m((pair[0].lat, pair[0].lon), (pair[1].lat, pair[1].lon))
            .hypot(pair[1].altitude_m - pair[0].altitude_m);
        if !leg.is_finite() {
            result
                .errors
                .push("leg distance cannot be calculated".into());
            result.leg_lengths_m.push(0.0);
            continue;
        }
        if leg > state.route_max_leg_m {
            result.errors.push("leg exceeds maximum distance".into());
        }
        result.leg_lengths_m.push(leg);
    }
    result.distance_m = result.leg_lengths_m.iter().sum();
    result.estimated_duration_s = result.distance_m / speed + hold_seconds;
    result.estimated_energy_wh = state.route_power_w * result.estimated_duration_s / 3600.0;
    if result.distance_m > state.route_max_distance_m {
        result.errors.push("route exceeds maximum distance".into());
    }
    if result.estimated_energy_wh > state.route_battery_wh {
        result.errors.push("route exceeds battery capacity".into());
    }
    if result.estimated_energy_wh > state.route_battery_wh * 0.8 {
        result
            .warnings
            .push("route uses more than 80% of battery".into());
    }
    for zone in zones {
        let polygon = serde_json::from_value::<Vec<Waypoint>>(zone.polygon.clone());
        let polygon = match polygon {
            Ok(p)
                if (3..=256).contains(&p.len())
                    && p.iter().all(|w| validate_lat_lon(w.lat, w.lon).is_ok())
                    && zone.max_altitude_m.is_none_or(|v| v.is_finite() && v > 0.0)
                    && p.iter()
                        .zip(p.iter().cycle().skip(1))
                        .all(|(a, b)| (a.lon - b.lon).abs() <= 180.0) =>
            {
                p
            }
            _ => {
                result
                    .errors
                    .push(format!("invalid active no-fly zone {}", zone.id));
                continue;
            }
        };
        if input
            .waypoints
            .windows(2)
            .any(|pair| segment_hits_zone(&pair[0], &pair[1], &polygon, zone.max_altitude_m))
        {
            result
                .errors
                .push(format!("route crosses no-fly zone '{}'", zone.name));
            result.no_fly_zone_conflicts.push(ZoneConflict {
                zone_id: zone.id,
                zone_name: zone.name.clone(),
                reason: "segment touches/intersects restricted zone below ceiling".into(),
            });
        }
    }
    result.valid = result.errors.is_empty();
    result
}

fn cross(a: (f64, f64), b: (f64, f64)) -> f64 {
    a.0 * b.1 - a.1 * b.0
}
fn point_on_segment(p: &Waypoint, a: &Waypoint, b: &Waypoint) -> bool {
    let d = (b.lon - a.lon, b.lat - a.lat);
    let q = (p.lon - a.lon, p.lat - a.lat);
    cross(d, q).abs() <= 1e-12
        && p.lon >= a.lon.min(b.lon) - 1e-12
        && p.lon <= a.lon.max(b.lon) + 1e-12
        && p.lat >= a.lat.min(b.lat) - 1e-12
        && p.lat <= a.lat.max(b.lat) + 1e-12
}
fn inside_or_boundary(p: &Waypoint, polygon: &[Waypoint]) -> bool {
    geo::point_in_polygon(p.lat, p.lon, polygon)
        || polygon
            .iter()
            .zip(polygon.iter().cycle().skip(1))
            .any(|(a, b)| point_on_segment(p, a, b))
}
/// Clip to the below-ceiling interval, then intersect every polygon edge exactly.
pub fn segment_hits_zone(
    a: &Waypoint,
    b: &Waypoint,
    polygon: &[Waypoint],
    ceiling: Option<f64>,
) -> bool {
    let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
    if let Some(c) = ceiling {
        if a.altitude_m > c && b.altitude_m > c {
            return false;
        }
        let dz = b.altitude_m - a.altitude_m;
        if dz > 0.0 {
            hi = hi.min((c - a.altitude_m) / dz);
        }
        if dz < 0.0 {
            lo = lo.max((c - a.altitude_m) / dz);
        }
        if lo > hi {
            return false;
        }
    }
    let at = |t: f64| Waypoint {
        lat: a.lat + (b.lat - a.lat) * t,
        lon: a.lon + (b.lon - a.lon) * t,
        altitude_m: a.altitude_m + (b.altitude_m - a.altitude_m) * t,
    };
    let p = at(lo);
    let q = at(hi);
    if inside_or_boundary(&p, polygon) || inside_or_boundary(&q, polygon) {
        return true;
    }
    let r = (q.lon - p.lon, q.lat - p.lat);
    polygon
        .iter()
        .zip(polygon.iter().cycle().skip(1))
        .any(|(c, d)| {
            let s = (d.lon - c.lon, d.lat - c.lat);
            let delta = (c.lon - p.lon, c.lat - p.lat);
            let den = cross(r, s);
            if den.abs() <= 1e-12 {
                return point_on_segment(c, &p, &q)
                    || point_on_segment(d, &p, &q)
                    || point_on_segment(&p, c, d)
                    || point_on_segment(&q, c, d);
            }
            let t = cross(delta, s) / den;
            let u = cross(delta, r) / den;
            (-1e-12..=1.0 + 1e-12).contains(&t) && (-1e-12..=1.0 + 1e-12).contains(&u)
        })
}

pub fn validate_lat_lon(lat: f64, lon: f64) -> Result<(), String> {
    if !(-90.0..=90.0).contains(&lat) {
        return Err("latitude must be between -90 and 90".into());
    }
    if !(-180.0..=180.0).contains(&lon) {
        return Err("longitude must be between -180 and 180".into());
    }
    Ok(())
}

pub(crate) async fn fetch_route(pool: &PgPool, id: Uuid) -> Result<Route, AppError> {
    sqlx::query_as::<_, Route>(
        r#"
        SELECT id, name, drone_id, waypoints, cruise_speed_mps, max_altitude_m,
               distance_m, estimated_duration_s, created_at, updated_at
        FROM neurovector.routes
        WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await?
    .ok_or(AppError::NotFound)
}
