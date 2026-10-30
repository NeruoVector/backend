use axum::{
    Extension, Json,
    extract::{Path, State},
};
use uuid::Uuid;

use super::super::owned_merchant;
use crate::{customer_auth::Customer, error::AppError, routes::inventory, state::AppState};
mod model;
use model::{
    CreateProduct, Product, UpdateProduct, fetch_product, nonempty, product_delete_error,
    require_category, validate_image_url,
};

fn validate_weight_grams(weight_grams: i32) -> Result<(), AppError> {
    if weight_grams <= 0 {
        return Err(AppError::BadRequest("weight_grams must be positive".into()));
    }
    Ok(())
}

pub(super) async fn list(
    State(state): State<AppState>,
    Extension(customer): Extension<Customer>,
) -> Result<Json<Vec<Product>>, AppError> {
    let merchant = owned_merchant(&state, &customer).await?;
    let products = crate::db::fetch_all::<Product>(
        &state.pool.get().await?,
        r#"
        SELECT i.id, d.merchant_id, d.category_id, i.sku, i.name, i.description,
               i.unit_price, i.currency, i.stock_quantity, i.active, d.image_url,
               d.weight_grams, i.created_at, i.updated_at
        FROM neurovector.inventory_items i
        JOIN neurovector.product_details d ON d.item_id=i.id
        WHERE d.merchant_id = $1
        ORDER BY i.name, i.id
        "#,
        &[&(merchant.id)],
    )
    .await?;
    Ok(Json(products))
}

pub(super) async fn create(
    State(state): State<AppState>,
    Extension(customer): Extension<Customer>,
    Json(body): Json<CreateProduct>,
) -> Result<(axum::http::StatusCode, Json<Product>), AppError> {
    let merchant = owned_merchant(&state, &customer).await?;
    let sku = nonempty(&body.sku, "sku")?;
    let name = nonempty(&body.name, "name")?;
    validate_weight_grams(body.weight_grams)?;
    validate_image_url(&body.image_url)?;
    let stock = body.stock_quantity.unwrap_or(0);
    inventory::validate_price_and_stock(body.unit_price, stock)?;
    let currency = inventory::normalize_currency(body.currency.as_deref().unwrap_or("KRW"))?;
    let mut connection = state.pool.get().await?;
    let mut tx = connection.transaction().await?;
    require_category(&mut tx, body.category_id, merchant.id).await?;
    let item_id: Uuid = crate::db::scalar_one(
        &tx,
        r#"
        INSERT INTO neurovector.inventory_items
            (sku, name, description, unit_price, currency, stock_quantity, active)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        RETURNING id
        "#,
        &[
            &(sku),
            &(name),
            &(body.description),
            &(body.unit_price),
            &(currency),
            &(stock),
            &(body.active),
        ],
    )
    .await
    .map_err(inventory::map_inventory_error)?;
    crate::db::execute(&tx, "INSERT INTO neurovector.product_details (item_id, merchant_id, category_id, image_url, weight_grams) VALUES ($1, $2, $3, $4, $5)", &[&(item_id), &(merchant.id), &(body.category_id), &(body.image_url.trim()), &(body.weight_grams)])
    .await?;
    let product = fetch_product(&mut tx, item_id, merchant.id).await?;
    tx.commit().await?;
    drop(connection);
    inventory::invalidate_inventory_cache(&state).await;
    Ok((axum::http::StatusCode::CREATED, Json(product)))
}

pub(super) async fn update(
    State(state): State<AppState>,
    Extension(customer): Extension<Customer>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateProduct>,
) -> Result<Json<Product>, AppError> {
    let merchant = owned_merchant(&state, &customer).await?;
    let mut connection = state.pool.get().await?;
    let mut tx = connection.transaction().await?;
    let current = fetch_product(&mut tx, id, merchant.id).await?;
    let name = body.name.unwrap_or(current.name);
    let description = body.description.or(current.description);
    let unit_price = body.unit_price.unwrap_or(current.unit_price);
    let currency =
        inventory::normalize_currency(body.currency.as_deref().unwrap_or(&current.currency))?;
    let stock = body.stock_quantity.unwrap_or(current.stock_quantity);
    let active = body.active.unwrap_or(current.active);
    let category_id = body.category_id.unwrap_or(current.category_id);
    let image_url = body.image_url.unwrap_or(current.image_url);
    let weight_grams = body.weight_grams.unwrap_or(current.weight_grams);
    let name = nonempty(&name, "name")?;
    validate_weight_grams(weight_grams)?;
    validate_image_url(&image_url)?;
    inventory::validate_price_and_stock(unit_price, stock)?;
    require_category(&mut tx, category_id, merchant.id).await?;
    crate::db::execute(&tx, "UPDATE neurovector.inventory_items SET name=$2, description=$3, unit_price=$4, currency=$5, stock_quantity=$6, active=$7, updated_at=now() WHERE id=$1", &[&(id), &(name), &(description), &(unit_price), &(currency), &(stock), &(active)])
    .await?;
    crate::db::execute(&tx, "UPDATE neurovector.product_details SET category_id=$3, image_url=$4, weight_grams=$5 WHERE item_id=$1 AND merchant_id=$2", &[&(id), &(merchant.id), &(category_id), &(image_url.trim()), &(weight_grams)])
    .await?;
    let product = fetch_product(&mut tx, id, merchant.id).await?;
    tx.commit().await?;
    drop(connection);
    inventory::invalidate_inventory_cache(&state).await;
    Ok(Json(product))
}

pub(super) async fn remove(
    State(state): State<AppState>,
    Extension(customer): Extension<Customer>,
    Path(id): Path<Uuid>,
) -> Result<axum::http::StatusCode, AppError> {
    let merchant = owned_merchant(&state, &customer).await?;
    let result = crate::db::execute(&state.pool.get().await?, "DELETE FROM neurovector.inventory_items i USING neurovector.product_details d WHERE i.id=d.item_id AND i.id=$1 AND d.merchant_id=$2", &[&(id), &(merchant.id)])
        .await
        .map_err(product_delete_error)?;
    if result == 0 {
        return Err(AppError::NotFound);
    }
    inventory::invalidate_inventory_cache(&state).await;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::validate_weight_grams;

    #[test]
    fn product_weight_must_be_positive() {
        assert!(validate_weight_grams(1).is_ok());
        assert!(validate_weight_grams(0).is_err());
        assert!(validate_weight_grams(-1).is_err());
    }
}
