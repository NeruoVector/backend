use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{error::AppError, state::AppState};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_items).post(create_item))
        .route("/{id}", get(get_item).patch(update_item))
        .route("/{id}/restock", post(restock_item))
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct InventoryItem {
    pub id: Uuid,
    pub sku: String,
    pub name: String,
    pub description: Option<String>,
    pub unit_price: i64,
    pub currency: String,
    pub stock_quantity: i32,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct ListItemsParams {
    #[serde(default)]
    pub active: Option<bool>,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct CreateItem {
    pub sku: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub unit_price: i64,
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub stock_quantity: Option<i32>,
    #[serde(default = "default_true")]
    pub active: bool,
}

#[derive(Debug, Deserialize)]
pub struct UpdateItem {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<Option<String>>,
    #[serde(default)]
    pub unit_price: Option<i64>,
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub stock_quantity: Option<i32>,
    #[serde(default)]
    pub active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct RestockItem {
    pub quantity: i32,
}

fn default_true() -> bool {
    true
}

async fn list_items(
    State(state): State<AppState>,
    Query(params): Query<ListItemsParams>,
) -> Result<Json<Vec<InventoryItem>>, AppError> {
    let limit = params.limit.unwrap_or(100).clamp(1, 500);
    let items = sqlx::query_as::<_, InventoryItem>(
        r#"
        SELECT id, sku, name, description, unit_price, currency, stock_quantity,
               active, created_at, updated_at
        FROM neurovector.inventory_items
        WHERE ($1::boolean IS NULL OR active = $1)
        ORDER BY name ASC
        LIMIT $2
        "#,
    )
    .bind(params.active)
    .bind(limit)
    .fetch_all(&state.pool)
    .await?;

    Ok(Json(items))
}

async fn get_item(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<InventoryItem>, AppError> {
    Ok(Json(fetch_item(&state, id).await?))
}

async fn create_item(
    State(state): State<AppState>,
    Json(body): Json<CreateItem>,
) -> Result<(StatusCode, Json<InventoryItem>), AppError> {
    if body.sku.trim().is_empty() {
        return Err(AppError::BadRequest("sku is required".into()));
    }
    if body.name.trim().is_empty() {
        return Err(AppError::BadRequest("name is required".into()));
    }
    validate_price_and_stock(body.unit_price, body.stock_quantity.unwrap_or(0))?;
    let currency = normalize_currency(body.currency.as_deref().unwrap_or("KRW"))?;

    let item = sqlx::query_as::<_, InventoryItem>(
        r#"
        INSERT INTO neurovector.inventory_items
            (sku, name, description, unit_price, currency, stock_quantity, active)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        RETURNING id, sku, name, description, unit_price, currency, stock_quantity,
                  active, created_at, updated_at
        "#,
    )
    .bind(body.sku.trim())
    .bind(body.name.trim())
    .bind(body.description)
    .bind(body.unit_price)
    .bind(currency)
    .bind(body.stock_quantity.unwrap_or(0))
    .bind(body.active)
    .fetch_one(&state.pool)
    .await
    .map_err(map_inventory_error)?;

    Ok((StatusCode::CREATED, Json(item)))
}

async fn update_item(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateItem>,
) -> Result<Json<InventoryItem>, AppError> {
    let current = fetch_item(&state, id).await?;
    let name = body.name.unwrap_or(current.name);
    let description = match body.description {
        Some(value) => value,
        None => current.description,
    };
    let unit_price = body.unit_price.unwrap_or(current.unit_price);
    let currency = normalize_currency(body.currency.as_deref().unwrap_or(&current.currency))?;
    let stock_quantity = body.stock_quantity.unwrap_or(current.stock_quantity);
    let active = body.active.unwrap_or(current.active);

    if name.trim().is_empty() {
        return Err(AppError::BadRequest("name is required".into()));
    }
    validate_price_and_stock(unit_price, stock_quantity)?;

    let item = sqlx::query_as::<_, InventoryItem>(
        r#"
        UPDATE neurovector.inventory_items
        SET name = $2, description = $3, unit_price = $4, currency = $5,
            stock_quantity = $6, active = $7, updated_at = now()
        WHERE id = $1
        RETURNING id, sku, name, description, unit_price, currency, stock_quantity,
                  active, created_at, updated_at
        "#,
    )
    .bind(id)
    .bind(name.trim())
    .bind(description)
    .bind(unit_price)
    .bind(currency)
    .bind(stock_quantity)
    .bind(active)
    .fetch_one(&state.pool)
    .await?;

    Ok(Json(item))
}

async fn restock_item(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<RestockItem>,
) -> Result<Json<InventoryItem>, AppError> {
    if body.quantity <= 0 {
        return Err(AppError::BadRequest("quantity must be positive".into()));
    }

    let item = sqlx::query_as::<_, InventoryItem>(
        r#"
        UPDATE neurovector.inventory_items
        SET stock_quantity = stock_quantity + $2, updated_at = now()
        WHERE id = $1
        RETURNING id, sku, name, description, unit_price, currency, stock_quantity,
                  active, created_at, updated_at
        "#,
    )
    .bind(id)
    .bind(body.quantity)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::NotFound)?;

    Ok(Json(item))
}

async fn fetch_item(state: &AppState, id: Uuid) -> Result<InventoryItem, AppError> {
    sqlx::query_as::<_, InventoryItem>(
        r#"
        SELECT id, sku, name, description, unit_price, currency, stock_quantity,
               active, created_at, updated_at
        FROM neurovector.inventory_items
        WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::NotFound)
}

fn validate_price_and_stock(unit_price: i64, stock_quantity: i32) -> Result<(), AppError> {
    if unit_price < 0 {
        return Err(AppError::BadRequest("unit_price must be >= 0".into()));
    }
    if stock_quantity < 0 {
        return Err(AppError::BadRequest("stock_quantity must be >= 0".into()));
    }
    Ok(())
}

fn normalize_currency(currency: &str) -> Result<String, AppError> {
    let currency = currency.trim().to_ascii_uppercase();
    if currency.len() != 3 || !currency.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(AppError::BadRequest(
            "currency must be a 3-letter code".into(),
        ));
    }
    Ok(currency)
}

fn map_inventory_error(err: sqlx::Error) -> AppError {
    if let sqlx::Error::Database(ref database_error) = err
        && database_error.is_unique_violation()
    {
        return AppError::Conflict("sku already exists".into());
    }
    AppError::Database(err)
}
