mod accounts;
pub mod config;
#[cfg(test)]
mod database_tests;
mod provider;
#[cfg(test)]
mod tests;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, header},
    response::{IntoResponse, Redirect, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    auth::{cookie_value, hash_token},
    error::AppError,
    state::AppState,
};
use config::Provider;

const FLOW_COOKIE: &str = "nv_oauth_flow";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/oauth/providers", get(providers))
        .route("/auth/oauth/{provider}/start", get(start))
        .route("/auth/oauth/{provider}/callback", get(callback))
}

#[derive(Serialize)]
struct ProviderStatus {
    provider: Provider,
    enabled: bool,
}

async fn providers(State(state): State<AppState>) -> (HeaderMap, Json<Vec<ProviderStatus>>) {
    let mut headers = HeaderMap::new();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    (
        headers,
        Json(
            Provider::ALL
                .into_iter()
                .map(|provider| ProviderStatus {
                    provider,
                    enabled: state.oauth.provider(provider).is_some(),
                })
                .collect(),
        ),
    )
}

fn flow_headers(token: &str, secure: bool, age: u32) -> Result<HeaderMap, AppError> {
    let mut headers = HeaderMap::new();
    let secure = if secure { "; Secure" } else { "" };
    headers.insert(header::SET_COOKIE, HeaderValue::from_str(&format!(
        "{FLOW_COOKIE}={token}; Path=/api/v1/auth/oauth; HttpOnly; SameSite=Lax; Max-Age={age}{secure}"
    )).map_err(anyhow::Error::from)?);
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    Ok(headers)
}

async fn start(
    State(state): State<AppState>,
    Path(provider): Path<Provider>,
) -> Result<Response, AppError> {
    let Some(config) = state.oauth.provider(provider) else {
        return Ok(Redirect::to(&state.oauth.return_url(Some("unavailable"))).into_response());
    };
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let verifier = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    sqlx::query("DELETE FROM neurovector.customer_oauth_flows WHERE expires_at<=now()")
        .execute(&state.pool)
        .await?;
    sqlx::query("INSERT INTO neurovector.customer_oauth_flows(state_hash,provider,verifier,expires_at) VALUES($1,$2,$3,now()+interval '10 minutes')")
        .bind(hash_token(&token)).bind(provider.name()).bind(&verifier).execute(&state.pool).await?;
    let url = provider::authorization_url(config, &token, &verifier).map_err(AppError::Other)?;
    Ok((
        flow_headers(
            &token,
            state.admin_cookie_secure || state.oauth.secure_cookies(),
            600,
        )?,
        Redirect::to(&url),
    )
        .into_response())
}

#[derive(Deserialize)]
struct Callback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

fn bound_state(headers: &HeaderMap, callback: &Callback) -> Option<String> {
    let token = cookie_value(headers, FLOW_COOKIE)?;
    if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    callback
        .state
        .as_ref()
        .filter(|received| **received == token)
        .map(|_| token)
}

async fn callback(
    State(state): State<AppState>,
    Path(provider): Path<Provider>,
    Query(query): Query<Callback>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let mut response_headers = flow_headers(
        "",
        state.admin_cookie_secure || state.oauth.secure_cookies(),
        0,
    )?;
    let outcome = complete(&state, provider, (&query, &headers)).await;
    let url = match outcome {
        Ok(session) => {
            for cookie in session.get_all(header::SET_COOKIE) {
                response_headers.append(header::SET_COOKIE, cookie.clone());
            }
            state.oauth.return_url(None)
        }
        Err(code) => state.oauth.return_url(Some(code)),
    };
    Ok((response_headers, Redirect::to(&url)).into_response())
}

async fn complete(
    state: &AppState,
    provider: Provider,
    request: (&Callback, &HeaderMap),
) -> Result<HeaderMap, &'static str> {
    let (query, headers) = request;
    let config = state.oauth.provider(provider).ok_or("unavailable")?;
    let token = bound_state(headers, query).ok_or("invalid_state")?;
    let verifier = accounts::consume_flow(state, provider, &token)
        .await
        .map_err(|_| "failed")?
        .ok_or("invalid_state")?;
    if query.error.is_some() {
        return Err("cancelled");
    }
    let code = query
        .code
        .as_ref()
        .filter(|s| !s.is_empty() && s.len() <= 4096)
        .ok_or("failed")?;
    let identity = provider::identity(provider::Exchange {
        config,
        code,
        state: &token,
        verifier: &verifier,
    })
    .await
    .map_err(|_| "failed")?;
    accounts::sign_in(state, provider, identity)
        .await
        .map_err(|_| "failed")
}
