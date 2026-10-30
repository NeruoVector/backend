use axum::{
    Extension, Json,
    extract::{Path, State},
};
use serde::{Deserialize, Serialize};

use uuid::Uuid;

use super::super::owned_merchant;
use crate::{customer_auth::Customer, error::AppError, state::AppState};

#[derive(Debug, Serialize)]
pub(super) struct Category {
    id: Uuid,
    merchant_id: Uuid,
    name: String,
}
crate::db::row!(Category {
    id,
    merchant_id,
    name
});

#[derive(Debug, Deserialize)]
pub(super) struct CategoryInput {
    name: String,
}

pub(super) async fn list(
    State(state): State<AppState>,
    Extension(customer): Extension<Customer>,
) -> Result<Json<Vec<Category>>, AppError> {
    let merchant = owned_merchant(&state, &customer).await?;
    let categories = crate::db::fetch_all::<Category>(&state.pool.get().await?, "SELECT id, merchant_id, name FROM neurovector.product_categories WHERE merchant_id = $1 ORDER BY name, id", &[&(merchant.id)])
    .await?;
    Ok(Json(categories))
}

pub(super) async fn create(
    State(state): State<AppState>,
    Extension(customer): Extension<Customer>,
    Json(body): Json<CategoryInput>,
) -> Result<Json<Category>, AppError> {
    let merchant = owned_merchant(&state, &customer).await?;
    let name = valid_name(&body.name)?;
    let category = crate::db::fetch_one::<Category>(&state.pool.get().await?, "INSERT INTO neurovector.product_categories (merchant_id, name) VALUES ($1, $2) RETURNING id, merchant_id, name", &[&(merchant.id), &(name)])
    .await
    .map_err(category_error)?;
    Ok(Json(category))
}

pub(super) async fn update(
    State(state): State<AppState>,
    Extension(customer): Extension<Customer>,
    Path(id): Path<Uuid>,
    Json(body): Json<CategoryInput>,
) -> Result<Json<Category>, AppError> {
    let merchant = owned_merchant(&state, &customer).await?;
    let name = valid_name(&body.name)?;
    let category = crate::db::fetch_optional::<Category>(&state.pool.get().await?, "UPDATE neurovector.product_categories SET name = $3 WHERE id = $1 AND merchant_id = $2 RETURNING id, merchant_id, name", &[&(id), &(merchant.id), &(name)])
    .await
    .map_err(category_error)?
    .ok_or(AppError::NotFound)?;
    Ok(Json(category))
}

pub(super) async fn remove(
    State(state): State<AppState>,
    Extension(customer): Extension<Customer>,
    Path(id): Path<Uuid>,
) -> Result<axum::http::StatusCode, AppError> {
    let merchant = owned_merchant(&state, &customer).await?;
    let result = crate::db::execute(
        &state.pool.get().await?,
        "DELETE FROM neurovector.product_categories WHERE id = $1 AND merchant_id = $2",
        &[&(id), &(merchant.id)],
    )
    .await
    .map_err(category_error)?;
    if result == 0 {
        return Err(AppError::NotFound);
    }
    Ok(axum::http::StatusCode::NO_CONTENT)
}

fn valid_name(value: &str) -> Result<&str, AppError> {
    let name = value.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(AppError::BadRequest(
            "category name must contain 1 to 100 characters".into(),
        ));
    }
    Ok(name)
}

fn category_error(error: tokio_postgres::Error) -> AppError {
    if let Some(database) = error.as_db_error()
        && database.code().code() == "23505"
    {
        return AppError::Conflict("category name already exists".into());
    }
    if let Some(database) = error.as_db_error()
        && database.code().code() == "23503"
    {
        return AppError::Conflict("category is still used by products".into());
    }
    AppError::Database(error)
}
