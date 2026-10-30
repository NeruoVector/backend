use axum::{
    Json, Router,
    extract::{Path, State},
    middleware,
    routing::{get, post},
};
use serde::Serialize;

use uuid::Uuid;

use crate::{auth, error::AppError, state::AppState};

#[derive(Debug, Serialize)]
struct CustomerRow {
    id: Uuid,
    name: String,
    provider: String,
    merchant_enabled: bool,
    student_status: Option<String>,
}
crate::db::row!(CustomerRow {
    id,
    name,
    provider,
    merchant_enabled,
    student_status
});

#[derive(Debug, Serialize)]
struct GrantedMerchant {
    id: Uuid,
    owner_id: Uuid,
    name: String,
    description: String,
    cover_url: String,
}
crate::db::row!(GrantedMerchant {
    id,
    owner_id,
    name,
    description,
    cover_url
});

pub(crate) fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/admin/customers", get(list_customers))
        .route("/admin/customers/{id}/merchant", post(grant_merchant))
        .route_layer(middleware::from_fn_with_state(state, auth::require_admin))
        .route_layer(middleware::from_fn(
            crate::customer_auth::require_same_origin,
        ))
}

async fn list_customers(State(state): State<AppState>) -> Result<Json<Vec<CustomerRow>>, AppError> {
    let customers = crate::db::fetch_all::<CustomerRow>(
        &state.pool.get().await?,
        r#"
        SELECT c.id, c.name, c.provider, c.merchant_enabled, v.status AS student_status
        FROM neurovector.customers c
        LEFT JOIN neurovector.student_verifications v ON v.customer_id = c.id
        ORDER BY c.name, c.id
        "#,
        &[],
    )
    .await?;
    Ok(Json(customers))
}

async fn grant_merchant(
    State(state): State<AppState>,
    Path(customer_id): Path<Uuid>,
) -> Result<Json<GrantedMerchant>, AppError> {
    let mut connection = state.pool.get().await?;
    let tx = connection.transaction().await?;
    let name = crate::db::scalar_optional::<String>(
        &tx,
        "UPDATE neurovector.customers SET merchant_enabled = true WHERE id = $1 RETURNING name",
        &[&(customer_id)],
    )
    .await?
    .ok_or(AppError::NotFound)?;
    let merchant = crate::db::fetch_one::<GrantedMerchant>(
        &tx,
        r#"
        INSERT INTO neurovector.merchants (owner_id, name)
        VALUES ($1, $2)
        ON CONFLICT (owner_id) DO UPDATE SET owner_id = EXCLUDED.owner_id
        RETURNING id, owner_id, name, description, cover_url
        "#,
        &[&(customer_id), &(name)],
    )
    .await?;
    tx.commit().await?;
    drop(connection);
    Ok(Json(merchant))
}
