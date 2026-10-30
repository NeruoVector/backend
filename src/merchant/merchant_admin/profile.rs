use axum::{Extension, Json, extract::State};
use serde::Deserialize;

use crate::merchant::{Merchant, owned_merchant};
use crate::{customer_auth::Customer, error::AppError, state::AppState};

#[derive(Debug, Deserialize)]
pub(super) struct UpdateProfile {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    cover_url: Option<String>,
}

pub(super) async fn get(
    State(state): State<AppState>,
    Extension(customer): Extension<Customer>,
) -> Result<Json<Merchant>, AppError> {
    Ok(Json(owned_merchant(&state, &customer).await?))
}

pub(super) async fn update(
    State(state): State<AppState>,
    Extension(customer): Extension<Customer>,
    Json(body): Json<UpdateProfile>,
) -> Result<Json<Merchant>, AppError> {
    let current = owned_merchant(&state, &customer).await?;
    let name = body.name.unwrap_or(current.name);
    let description = body.description.unwrap_or(current.description);
    let cover_url = body.cover_url.unwrap_or(current.cover_url);
    if name.trim().is_empty() || name.trim().chars().count() > 100 {
        return Err(AppError::BadRequest(
            "name must contain 1 to 100 characters".into(),
        ));
    }
    if description.chars().count() > 3000 {
        return Err(AppError::BadRequest(
            "description must be at most 3000 characters".into(),
        ));
    }
    validate_image_url(&cover_url)?;
    let merchant = crate::db::fetch_optional::<Merchant>(
        &state.pool.get().await?,
        r#"
        UPDATE neurovector.merchants
        SET name = $2, description = $3, cover_url = $4
        WHERE id = $1 AND owner_id = $5
        RETURNING id, owner_id, name, description, cover_url
        "#,
        &[
            &(current.id),
            &(name.trim()),
            &(description.trim()),
            &(cover_url.trim()),
            &(customer.id),
        ],
    )
    .await?
    .ok_or(AppError::NotFound)?;
    Ok(Json(merchant))
}

pub(super) fn validate_image_url(value: &str) -> Result<(), AppError> {
    if value.is_empty() {
        return Ok(());
    }
    let url = reqwest::Url::parse(value)
        .map_err(|_| AppError::BadRequest("image URL must be absolute HTTP(S)".into()))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(AppError::BadRequest(
            "image URL must be absolute HTTP(S)".into(),
        ));
    }
    Ok(())
}
