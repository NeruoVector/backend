use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::get,
};
use uuid::Uuid;

use crate::{customer_auth, error::AppError, state::AppState};

mod types;

pub use types::{Spot, SpotInput};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/account/spots", get(list_spots).post(create_spot))
        .route(
            "/account/spots/{id}",
            get(get_spot).patch(update_spot).delete(delete_spot),
        )
}

async fn list_spots(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Vec<Spot>>, AppError> {
    let customer = customer_auth::current(&state, &headers).await?;
    let spots = crate::db::fetch_all::<Spot>(
        &state.pool.get().await?,
        r#"
        SELECT id, label, delivery_address, delivery_detail, latitude, longitude,
               room_number, grade, classroom, created_at, updated_at
        FROM neurovector.delivery_spots
        WHERE customer_id = $1
        ORDER BY created_at DESC, id
        "#,
        &[&(customer.id)],
    )
    .await?;
    Ok(Json(spots))
}

async fn create_spot(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(input): Json<SpotInput>,
) -> Result<(StatusCode, Json<Spot>), AppError> {
    let customer = customer_auth::current(&state, &headers).await?;
    let fields = input.into_fields()?;
    let spot = crate::db::fetch_one::<Spot>(
        &state.pool.get().await?,
        r#"
        INSERT INTO neurovector.delivery_spots
            (customer_id, label, delivery_address, delivery_detail, latitude, longitude,
             room_number, grade, classroom)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        RETURNING id, label, delivery_address, delivery_detail, latitude, longitude,
                  room_number, grade, classroom, created_at, updated_at
        "#,
        &[
            &(customer.id),
            &(fields.label),
            &(fields.delivery_address),
            &(fields.delivery_detail),
            &(fields.latitude),
            &(fields.longitude),
            &(fields.room_number),
            &(fields.grade),
            &(fields.classroom),
        ],
    )
    .await?;
    Ok((StatusCode::CREATED, Json(spot)))
}

async fn get_spot(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Spot>, AppError> {
    let customer = customer_auth::current(&state, &headers).await?;
    let spot = crate::db::fetch_optional::<Spot>(
        &state.pool.get().await?,
        r#"
        SELECT id, label, delivery_address, delivery_detail, latitude, longitude,
               room_number, grade, classroom, created_at, updated_at
        FROM neurovector.delivery_spots
        WHERE id = $1 AND customer_id = $2
        "#,
        &[&(id), &(customer.id)],
    )
    .await?
    .ok_or(AppError::NotFound)?;
    Ok(Json(spot))
}

async fn update_spot(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
    Json(input): Json<SpotInput>,
) -> Result<Json<Spot>, AppError> {
    let customer = customer_auth::current(&state, &headers).await?;
    let fields = input.into_fields()?;
    let spot = crate::db::fetch_optional::<Spot>(
        &state.pool.get().await?,
        r#"
        UPDATE neurovector.delivery_spots
        SET label = $3, delivery_address = $4, delivery_detail = $5,
            latitude = $6, longitude = $7, room_number = $8,
            grade = $9, classroom = $10, updated_at = now()
        WHERE id = $1 AND customer_id = $2
        RETURNING id, label, delivery_address, delivery_detail, latitude, longitude,
                  room_number, grade, classroom, created_at, updated_at
        "#,
        &[
            &(id),
            &(customer.id),
            &(fields.label),
            &(fields.delivery_address),
            &(fields.delivery_detail),
            &(fields.latitude),
            &(fields.longitude),
            &(fields.room_number),
            &(fields.grade),
            &(fields.classroom),
        ],
    )
    .await?
    .ok_or(AppError::NotFound)?;
    Ok(Json(spot))
}

async fn delete_spot(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let customer = customer_auth::current(&state, &headers).await?;
    let result = crate::db::execute(
        &state.pool.get().await?,
        "DELETE FROM neurovector.delivery_spots WHERE id = $1 AND customer_id = $2",
        &[&(id), &(customer.id)],
    )
    .await?;
    if result == 0 {
        return Err(AppError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}
