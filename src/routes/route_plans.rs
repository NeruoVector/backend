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
        .route("/{id}", get(get_route).delete(delete_route))
        .route("/{id}/validation", get(validate_saved_route))
        .route(
            "/no-fly-zones",
            get(list_no_fly_zones).post(create_no_fly_zone),
        )
        .route("/no-fly-zones/{id}", delete(delete_no_fly_zone))
}

#[derive(Debug, Serialize, sqlx::FromRow)]
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

#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Serialize)]
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

#[derive(Debug, Serialize)]
pub struct ZoneConflict {
    pub zone_id: Uuid,
    pub zone_name: String,
    pub reason: String,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
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

#[derive(Debug, sqlx::FromRow)]
struct NoFlyZoneRow {
    id: Uuid,
    name: String,
    polygon: Value,
    max_altitude_m: Option<f64>,
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
    if body.polygon.len() < 3 {
        return Err(AppError::BadRequest(
            "polygon must have at least 3 points".into(),
        ));
    }
    for point in &body.polygon {
        validate_lat_lon(point.lat, point.lon).map_err(AppError::BadRequest)?;
    }
    if let Some(max_altitude_m) = body.max_altitude_m
        && max_altitude_m <= 0.0
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

pub async fn validate_route(
    state: &AppState,
    input: &RouteInput,
) -> Result<RouteValidation, AppError> {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    if input.name.trim().is_empty() {
        errors.push("name is required".to_string());
    }
    if input.waypoints.len() < 2 {
        errors.push("at least 2 waypoints are required".to_string());
    }

    let cruise_speed_mps = input.cruise_speed_mps.unwrap_or(8.0);
    if cruise_speed_mps <= 0.0 {
        errors.push("cruise_speed_mps must be positive".to_string());
    }
    let max_altitude_m = input.max_altitude_m.unwrap_or(state.max_altitude_m);
    if max_altitude_m <= 0.0 || max_altitude_m > state.max_altitude_m {
        errors.push(format!(
            "max_altitude_m must be between 0 and {}",
            state.max_altitude_m
        ));
    }

    let mut leg_lengths_m = Vec::new();
    let mut previous: Option<&Waypoint> = None;
    for (index, waypoint) in input.waypoints.iter().enumerate() {
        if let Err(err) = validate_lat_lon(waypoint.lat, waypoint.lon) {
            errors.push(format!("waypoint {index}: {err}"));
        }
        if waypoint.altitude_m <= 0.0 {
            errors.push(format!("waypoint {index}: altitude_m must be positive"));
        }
        if waypoint.altitude_m > max_altitude_m {
            errors.push(format!(
                "waypoint {index}: altitude_m {} exceeds limit {max_altitude_m}",
                waypoint.altitude_m
            ));
        }
        if let Some(previous) = previous {
            let leg = geo::haversine_m((previous.lat, previous.lon), (waypoint.lat, waypoint.lon));
            if leg < 1.0 {
                warnings.push(format!(
                    "waypoint {index}: distance from previous point is < 1m"
                ));
            }
            if leg > state.route_max_leg_m {
                errors.push(format!(
                    "waypoint {index}: leg {leg:.0}m exceeds maximum {}m",
                    state.route_max_leg_m
                ));
            }
            leg_lengths_m.push(leg);
        }
        previous = Some(waypoint);
    }

    let points = geo::waypoints_to_points(&input.waypoints);
    let distance_m = geo::route_distance_m(&points);
    if distance_m > state.route_max_distance_m {
        errors.push(format!(
            "route distance {distance_m:.0}m exceeds maximum {}m",
            state.route_max_distance_m
        ));
    }
    let estimated_duration_s = if cruise_speed_mps > 0.0 {
        distance_m / cruise_speed_mps
    } else {
        0.0
    };
    let estimated_energy_wh = state.route_power_w * estimated_duration_s / 3600.0;
    if estimated_energy_wh > state.route_battery_wh {
        errors.push(format!(
            "estimated energy {estimated_energy_wh:.1}Wh exceeds battery capacity {}Wh",
            state.route_battery_wh
        ));
    } else if estimated_energy_wh > state.route_battery_wh * 0.8 {
        warnings.push(format!(
            "estimated energy {estimated_energy_wh:.1}Wh uses more than 80% of battery"
        ));
    }
    if input
        .waypoints
        .iter()
        .any(|w| w.altitude_m > state.max_altitude_m)
    {
        // 개별 waypoint 검사에서 이미 오류를 추가했지만, 상한을 별도로 명시한다.
        warnings.push(format!("legal altitude limit is {}m", state.max_altitude_m));
    }

    let zones = sqlx::query_as::<_, NoFlyZoneRow>(
        r#"
        SELECT id, name, polygon, max_altitude_m
        FROM neurovector.no_fly_zones
        WHERE active = true
        "#,
    )
    .fetch_all(&state.pool)
    .await?;

    let samples = sample_route_with_altitude(&input.waypoints, 100.0);
    let mut no_fly_zone_conflicts = Vec::new();
    for zone in zones {
        let polygon = match serde_json::from_value::<Vec<Waypoint>>(zone.polygon) {
            Ok(polygon) => polygon,
            Err(err) => {
                warnings.push(format!("zone {} has invalid polygon: {err}", zone.name));
                continue;
            }
        };

        let conflict = samples.iter().find(|sample| {
            let inside = geo::point_in_polygon(sample.lat, sample.lon, &polygon);
            let below_zone_ceiling = zone
                .max_altitude_m
                .is_none_or(|ceiling| sample.altitude_m <= ceiling);
            inside && below_zone_ceiling
        });

        if conflict.is_some() {
            errors.push(format!("route crosses no-fly zone '{}'", zone.name));
            no_fly_zone_conflicts.push(ZoneConflict {
                zone_id: zone.id,
                zone_name: zone.name,
                reason: "route intersects the zone below its altitude ceiling".to_string(),
            });
        }
    }

    Ok(RouteValidation {
        valid: errors.is_empty(),
        errors,
        warnings,
        waypoint_count: input.waypoints.len(),
        distance_m,
        estimated_duration_s,
        estimated_energy_wh,
        max_altitude_m,
        leg_lengths_m,
        no_fly_zone_conflicts,
    })
}

fn sample_route_with_altitude(waypoints: &[Waypoint], max_step_m: f64) -> Vec<Waypoint> {
    if waypoints.len() < 2 {
        return waypoints.to_vec();
    }

    let mut samples = Vec::new();
    for pair in waypoints.windows(2) {
        let (start, end) = (&pair[0], &pair[1]);
        let distance = geo::haversine_m((start.lat, start.lon), (end.lat, end.lon));
        let steps = ((distance / max_step_m).ceil() as usize).max(1);
        for step in 0..steps {
            let t = step as f64 / steps as f64;
            samples.push(Waypoint {
                lat: start.lat + (end.lat - start.lat) * t,
                lon: start.lon + (end.lon - start.lon) * t,
                altitude_m: start.altitude_m + (end.altitude_m - start.altitude_m) * t,
            });
        }
    }
    if let Some(last) = waypoints.last() {
        samples.push(last.clone());
    }
    samples
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
