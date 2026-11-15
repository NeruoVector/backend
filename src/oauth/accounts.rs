use axum::http::HeaderMap;
use uuid::Uuid;

use super::{config::Provider, provider::Identity};
use crate::{
    auth::{hash_password_async, hash_token},
    error::AppError,
    state::AppState,
};

pub async fn sign_in(
    state: &AppState,
    provider: Provider,
    identity: Identity,
) -> Result<HeaderMap, AppError> {
    let mut tx = state.pool.begin().await?;
    let key = format!("{}:{}", provider.name(), identity.subject);
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(&key)
        .execute(&mut *tx)
        .await?;
    let existing: Option<Uuid> = sqlx::query_scalar("SELECT user_id FROM neurovector.customer_oauth_identities WHERE provider=$1 AND subject=$2")
        .bind(provider.name()).bind(&identity.subject).fetch_optional(&mut *tx).await?;
    let user = match existing {
        Some(user) => user,
        None => {
            let user = Uuid::new_v4();
            let username = format!("social_{}", &user.simple().to_string()[..24]);
            let password_hash =
                hash_password_async(format!("{}{}", Uuid::new_v4(), Uuid::new_v4())).await?;
            let name: String = identity.name.trim().chars().take(50).collect();
            let name = if name.is_empty() {
                format!("{} 사용자", provider.name())
            } else {
                name
            };
            sqlx::query("INSERT INTO neurovector.customer_users(id,username,password_hash,name,phone) VALUES($1,$2,$3,$4,'')")
                .bind(user).bind(username).bind(password_hash).bind(name).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO neurovector.customer_oauth_identities(provider,subject,user_id) VALUES($1,$2,$3)")
                .bind(provider.name()).bind(identity.subject).bind(user).execute(&mut *tx).await?;
            user
        }
    };
    let headers = crate::customer_auth::session_in_tx(
        &mut tx,
        user,
        state.admin_cookie_secure || state.oauth.secure_cookies(),
    )
    .await?;
    tx.commit().await?;
    Ok(headers)
}

pub async fn consume_flow(
    state: &AppState,
    provider: Provider,
    token: &str,
) -> Result<Option<String>, AppError> {
    Ok(sqlx::query_scalar("DELETE FROM neurovector.customer_oauth_flows WHERE state_hash=$1 AND provider=$2 AND expires_at>now() RETURNING verifier")
        .bind(hash_token(token)).bind(provider.name()).fetch_optional(&state.pool).await?)
}
