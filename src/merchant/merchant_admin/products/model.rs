use serde::{Deserialize, Serialize};

use uuid::Uuid;

use crate::error::AppError;

#[derive(Debug, Serialize)]
pub(in crate::merchant::merchant_admin) struct Product {
    pub(super) id: Uuid,
    pub(super) merchant_id: Uuid,
    pub(super) category_id: Uuid,
    pub(super) sku: String,
    pub(super) name: String,
    pub(super) description: Option<String>,
    pub(super) unit_price: i64,
    pub(super) currency: String,
    pub(super) stock_quantity: i32,
    pub(super) active: bool,
    pub(super) image_url: String,
    pub(super) weight_grams: i32,
    pub(super) created_at: chrono::DateTime<chrono::Utc>,
    pub(super) updated_at: chrono::DateTime<chrono::Utc>,
}
crate::db::row!(Product {
    id,
    merchant_id,
    category_id,
    sku,
    name,
    description,
    unit_price,
    currency,
    stock_quantity,
    active,
    image_url,
    weight_grams,
    created_at,
    updated_at
});

#[derive(Debug, Deserialize)]
pub(in crate::merchant::merchant_admin) struct CreateProduct {
    pub(super) sku: String,
    pub(super) name: String,
    #[serde(default)]
    pub(super) description: Option<String>,
    pub(super) unit_price: i64,
    #[serde(default)]
    pub(super) currency: Option<String>,
    #[serde(default)]
    pub(super) stock_quantity: Option<i32>,
    #[serde(default = "default_true")]
    pub(super) active: bool,
    pub(super) category_id: Uuid,
    #[serde(default)]
    pub(super) image_url: String,
    #[serde(default)]
    pub(super) weight_grams: i32,
}

#[derive(Debug, Deserialize)]
pub(in crate::merchant::merchant_admin) struct UpdateProduct {
    #[serde(default)]
    pub(super) name: Option<String>,
    #[serde(default)]
    pub(super) description: Option<String>,
    #[serde(default)]
    pub(super) unit_price: Option<i64>,
    #[serde(default)]
    pub(super) currency: Option<String>,
    #[serde(default)]
    pub(super) stock_quantity: Option<i32>,
    #[serde(default)]
    pub(super) active: Option<bool>,
    #[serde(default)]
    pub(super) category_id: Option<Uuid>,
    #[serde(default)]
    pub(super) image_url: Option<String>,
    #[serde(default)]
    pub(super) weight_grams: Option<i32>,
}

pub(super) async fn fetch_product(
    tx: &mut deadpool_postgres::Transaction<'_>,
    id: Uuid,
    merchant_id: Uuid,
) -> Result<Product, AppError> {
    crate::db::fetch_optional::<Product>(
        &*tx,
        r#"
        SELECT i.id, d.merchant_id, d.category_id, i.sku, i.name, i.description,
               i.unit_price, i.currency, i.stock_quantity, i.active, d.image_url,
               d.weight_grams, i.created_at, i.updated_at
        FROM neurovector.inventory_items i
        JOIN neurovector.product_details d ON d.item_id=i.id
        WHERE i.id=$1 AND d.merchant_id=$2
        FOR UPDATE OF i, d
        "#,
        &[&(id), &(merchant_id)],
    )
    .await?
    .ok_or(AppError::NotFound)
}

pub(super) async fn require_category(
    tx: &mut deadpool_postgres::Transaction<'_>,
    category_id: Uuid,
    merchant_id: Uuid,
) -> Result<(), AppError> {
    let exists = crate::db::scalar_one::<bool>(&*tx, "SELECT EXISTS (SELECT 1 FROM neurovector.product_categories WHERE id=$1 AND merchant_id=$2)", &[&(category_id), &(merchant_id)])
    .await?;
    if !exists {
        return Err(AppError::BadRequest(
            "category does not belong to this merchant".into(),
        ));
    }
    Ok(())
}

pub(super) fn nonempty<'a>(value: &'a str, field: &str) -> Result<&'a str, AppError> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 160 {
        return Err(AppError::BadRequest(format!(
            "{field} must contain 1 to 160 characters"
        )));
    }
    Ok(value)
}

pub(super) fn validate_image_url(value: &str) -> Result<(), AppError> {
    if value.is_empty() {
        return Ok(());
    }
    let url = reqwest::Url::parse(value)
        .map_err(|_| AppError::BadRequest("image_url must be absolute HTTP(S)".into()))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(AppError::BadRequest(
            "image_url must be absolute HTTP(S)".into(),
        ));
    }
    Ok(())
}

pub(super) fn product_delete_error(error: tokio_postgres::Error) -> AppError {
    if let Some(database) = error.as_db_error()
        && database.code().code() == "23503"
    {
        return AppError::Conflict("product is referenced by an existing order".into());
    }
    AppError::Database(error)
}

fn default_true() -> bool {
    true
}
