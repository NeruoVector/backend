use axum::{
    Json, Router,
    extract::{Query, State},
    routing::{get, patch, put},
};
use serde::{Deserialize, Serialize};

use uuid::Uuid;

use crate::{error::AppError, state::AppState};

mod categories;
mod products;
mod profile;

#[derive(Debug, Serialize)]
pub(super) struct CatalogProduct {
    pub id: Uuid,
    pub sku: String,
    pub name: String,
    pub description: Option<String>,
    pub unit_price: i64,
    pub currency: String,
    pub stock_quantity: i32,
    pub active: bool,
    pub merchant_id: Uuid,
    pub merchant_name: String,
    pub category_id: Uuid,
    pub category_name: String,
    pub image_url: String,
    pub weight_grams: i32,
}
crate::db::row!(CatalogProduct {
    id,
    sku,
    name,
    description,
    unit_price,
    currency,
    stock_quantity,
    active,
    merchant_id,
    merchant_name,
    category_id,
    category_name,
    image_url,
    weight_grams
});

#[derive(Debug, Deserialize)]
pub(super) struct CatalogParams {
    #[serde(default)]
    category_id: Option<Uuid>,
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
}

pub(super) fn profile_router() -> Router<AppState> {
    Router::new().route("/merchant/profile", get(profile::get).put(profile::update))
}

pub(super) fn categories_router() -> Router<AppState> {
    Router::new()
        .route(
            "/merchant/categories",
            get(categories::list).post(categories::create),
        )
        .route(
            "/merchant/categories/{id}",
            patch(categories::update).delete(categories::remove),
        )
}

pub(super) fn products_router() -> Router<AppState> {
    Router::new()
        .route(
            "/merchant/products",
            get(products::list).post(products::create),
        )
        .route(
            "/merchant/products/{id}",
            put(products::update).delete(products::remove),
        )
}

pub(super) async fn catalog(
    State(state): State<AppState>,
    Query(params): Query<CatalogParams>,
) -> Result<Json<Vec<CatalogProduct>>, AppError> {
    let limit = params.limit.unwrap_or(100).clamp(1, 500);
    let query = params
        .q
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let products = crate::db::fetch_all::<CatalogProduct>(&state.pool.get().await?, r#"
        SELECT i.id, i.sku, i.name, i.description, i.unit_price, i.currency,
               i.stock_quantity, i.active, m.id AS merchant_id, m.name AS merchant_name,
               c.id AS category_id, c.name AS category_name, d.image_url, d.weight_grams
        FROM neurovector.inventory_items i
        JOIN neurovector.product_details d ON d.item_id = i.id
        JOIN neurovector.merchants m ON m.id = d.merchant_id
        JOIN neurovector.product_categories c ON c.id = d.category_id AND c.merchant_id = d.merchant_id
        WHERE i.active = true
          AND ($1::uuid IS NULL OR c.id = $1)
          AND ($2::text IS NULL OR i.name ILIKE '%' || $2 || '%' OR i.description ILIKE '%' || $2 || '%')
        ORDER BY i.name, i.id
        LIMIT $3
        "#, &[&(params.category_id), &(query), &(limit)])
    .await?;
    Ok(Json(products))
}
