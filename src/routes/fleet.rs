//! Admin-authenticated fleet configuration and payload measurement APIs.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    middleware,
    routing::{get, post},
};

use crate::{auth, error::AppError, state::AppState};

mod measurements;
mod types;

#[allow(unused_imports)]
pub use types::{FleetDrone, FleetDroneInput, PayloadMeasurement, PayloadMeasurementInput};

pub fn router(state: AppState) -> Router<AppState> {
    let protected = Router::new()
        .route("/admin/fleet", get(list_fleet).post(upsert_fleet_drone))
        .route(
            "/admin/fleet/{drone_id}",
            axum::routing::delete(deactivate_fleet_drone),
        )
        .route(
            "/admin/fleet/{drone_id}/payload-measurements",
            post(measurements::record_payload_measurement),
        )
        .route_layer(middleware::from_fn_with_state(state, auth::require_admin));
    Router::new().merge(protected)
}

async fn list_fleet(State(state): State<AppState>) -> Result<Json<Vec<FleetDrone>>, AppError> {
    let fleet = crate::db::fetch_all::<FleetDrone>(
        &state.pool.get().await?,
        r#"
        SELECT drone_id, capacity_grams, home_latitude, home_longitude,
               cruise_speed_mps, max_altitude_m, active, created_at, updated_at
        FROM neurovector.delivery_fleet
        ORDER BY drone_id
        "#,
        &[],
    )
    .await?;
    Ok(Json(fleet))
}

async fn upsert_fleet_drone(
    State(state): State<AppState>,
    Json(mut body): Json<FleetDroneInput>,
) -> Result<(StatusCode, Json<FleetDrone>), AppError> {
    body.validate()?;
    let drone = crate::db::fetch_one::<FleetDrone>(
        &state.pool.get().await?,
        r#"
        INSERT INTO neurovector.delivery_fleet
            (drone_id, capacity_grams, home_latitude, home_longitude,
             cruise_speed_mps, max_altitude_m, active)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        ON CONFLICT (drone_id) DO UPDATE
        SET capacity_grams = EXCLUDED.capacity_grams,
            home_latitude = EXCLUDED.home_latitude,
            home_longitude = EXCLUDED.home_longitude,
            cruise_speed_mps = EXCLUDED.cruise_speed_mps,
            max_altitude_m = EXCLUDED.max_altitude_m,
            active = EXCLUDED.active,
            updated_at = now()
        RETURNING drone_id, capacity_grams, home_latitude, home_longitude,
                  cruise_speed_mps, max_altitude_m, active, created_at, updated_at
        "#,
        &[
            &(body.drone_id),
            &(body.capacity_grams),
            &(body.home_latitude),
            &(body.home_longitude),
            &(body.cruise_speed_mps),
            &(body.max_altitude_m),
            &(body.active),
        ],
    )
    .await?;
    Ok((StatusCode::OK, Json(drone)))
}

async fn deactivate_fleet_drone(
    State(state): State<AppState>,
    Path(drone_id): Path<String>,
) -> Result<StatusCode, AppError> {
    let result = crate::db::execute(&state.pool.get().await?, "UPDATE neurovector.delivery_fleet SET active = false, updated_at = now() WHERE drone_id = $1", &[&(drone_id)])
    .await?;
    if result == 0 {
        return Err(AppError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}
