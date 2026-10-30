use axum::{Router, extract::State, middleware, routing::get};
use serde::Serialize;

use uuid::Uuid;

use crate::{customer_auth, error::AppError, state::AppState};

mod food_reference;
mod merchant_admin;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Merchant {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub name: String,
    pub description: String,
    pub cover_url: String,
}
crate::db::row!(Merchant {
    id,
    owner_id,
    name,
    description,
    cover_url
});

#[derive(Debug, Serialize)]
struct MerchantSummary {
    id: Uuid,
    name: String,
    description: String,
    cover_url: String,
}
crate::db::row!(MerchantSummary {
    id,
    name,
    description,
    cover_url
});

pub(crate) async fn owned_merchant(
    state: &AppState,
    customer: &customer_auth::Customer,
) -> Result<Merchant, AppError> {
    if !customer.merchant_enabled {
        return Err(AppError::Unauthorized("merchant access is required".into()));
    }
    crate::db::fetch_optional::<Merchant>(&state.pool.get().await?, "SELECT id, owner_id, name, description, cover_url FROM neurovector.merchants WHERE owner_id = $1", &[&(customer.id)])
    .await?
    .ok_or_else(|| AppError::Unauthorized("merchant profile is unavailable".into()))
}

pub fn router(state: AppState) -> Router<AppState> {
    let protected = Router::new()
        .merge(merchant_admin::profile_router())
        .merge(merchant_admin::categories_router())
        .merge(merchant_admin::products_router())
        .route("/merchant/food-reference", get(food_reference::search))
        .route_layer(middleware::from_fn_with_state(
            state,
            customer_auth::require_customer,
        ))
        .route_layer(middleware::from_fn(customer_auth::require_same_origin));
    Router::new()
        .route("/merchants", get(list_merchants))
        .route("/catalog", get(merchant_admin::catalog))
        .merge(protected)
}

async fn list_merchants(
    State(state): State<AppState>,
) -> Result<axum::Json<Vec<MerchantSummary>>, AppError> {
    let merchants = crate::db::fetch_all::<MerchantSummary>(
        &state.pool.get().await?,
        "SELECT id, name, description, cover_url FROM neurovector.merchants ORDER BY name, id",
        &[],
    )
    .await?;
    Ok(axum::Json(merchants))
}
