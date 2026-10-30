use crate::{
    customer_auth::{self, Customer},
    error::AppError,
    oauth_provider::{self, AccessToken, Provider},
    state::AppState,
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::Redirect,
    routing::{get, post},
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/providers", get(providers))
        .route("/auth/{provider}/login", get(login))
        .route("/auth/{provider}/callback", get(callback))
        .route("/auth/me", get(me))
        .route(
            "/auth/logout",
            post(logout).route_layer(axum::middleware::from_fn(
                customer_auth::require_same_origin,
            )),
        )
}
async fn providers() -> Json<Vec<&'static str>> {
    Json(
        ["google", "kakao", "naver"]
            .into_iter()
            .filter(|name| Provider::load(name).and_then(|p| p.redirect_uri()).is_ok())
            .collect(),
    )
}
async fn login(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<(HeaderMap, Redirect), AppError> {
    let p = Provider::load(&name)?;
    let nonce = customer_auth::token();
    crate::db::execute(
        &state.pool.get().await?,
        "DELETE FROM neurovector.oauth_states WHERE expires_at<now()",
        &[],
    )
    .await?;
    crate::db::execute(
        &state.pool.get().await?,
        "INSERT INTO neurovector.oauth_states VALUES ($1,$2,now()+interval '10 minutes')",
        &[&(customer_auth::digest(&nonce)), &(&name)],
    )
    .await?;
    let mut url = reqwest::Url::parse(p.authorize).map_err(anyhow::Error::from)?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("response_type", "code");
        query.append_pair("client_id", &p.client_id);
        query.append_pair("redirect_uri", &p.redirect_uri()?);
        query.append_pair("state", &nonce);
        if !p.scope.is_empty() {
            query.append_pair("scope", p.scope);
        }
    }
    let mut headers = HeaderMap::new();
    headers.insert(
        header::SET_COOKIE,
        customer_auth::cookie("nv_oauth", &nonce, 600)?,
    );
    headers.insert(
        header::CACHE_CONTROL,
        "no-store".parse().map_err(anyhow::Error::from)?,
    );
    Ok((headers, Redirect::to(url.as_str())))
}
#[derive(Deserialize)]
struct Callback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}
async fn callback(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Query(query): Query<Callback>,
) -> Result<(HeaderMap, Redirect), AppError> {
    let p = Provider::load(&name)?;
    let nonce = customer_auth::cookie_value(&headers, "nv_oauth")
        .ok_or_else(|| AppError::Unauthorized("로그인을 다시 시작하세요.".into()))?;
    let Some(returned_state) = query.state.as_deref() else {
        return Err(AppError::Unauthorized("OAuth state 불일치".into()));
    };
    let cookie_digest = Sha256::digest(nonce.as_bytes());
    let query_digest = Sha256::digest(returned_state.as_bytes());
    if !bool::from(cookie_digest.as_slice().ct_eq(query_digest.as_slice())) {
        return Err(AppError::Unauthorized("OAuth state 불일치".into()));
    }
    let consumed = crate::db::execute(&state.pool.get().await?, "DELETE FROM neurovector.oauth_states WHERE token_hash=$1 AND provider=$2 AND expires_at>now()", &[&(customer_auth::digest(&nonce)), &(&name)]).await?;
    if consumed != 1 {
        return Err(AppError::Unauthorized("로그인 요청 만료".into()));
    }
    let mut output = HeaderMap::new();
    output.append(
        header::SET_COOKIE,
        customer_auth::cookie("nv_oauth", "", 0)?,
    );
    output.insert(
        header::CACHE_CONTROL,
        "no-store".parse().map_err(anyhow::Error::from)?,
    );
    if query.error.is_some() {
        return Ok((output, store_redirect()?));
    }
    let code = query
        .code
        .ok_or_else(|| AppError::BadRequest("authorization code 없음".into()))?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(anyhow::Error::from)?;
    let token: AccessToken = client
        .post(p.token_url)
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", &p.client_id),
            ("client_secret", &p.secret),
            ("redirect_uri", &p.redirect_uri()?),
            ("code", &code),
            ("state", &nonce),
        ])
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|_| AppError::Unauthorized("소셜 토큰 교환 실패".into()))?
        .json()
        .await
        .map_err(|_| AppError::Unauthorized("소셜 토큰 응답 오류".into()))?;
    let profile = client
        .get(p.profile_url)
        .bearer_auth(token.access_token)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|_| AppError::Unauthorized("프로필 요청 실패".into()))?;
    let (subject, display_name) = oauth_provider::identity(&p, profile).await?;
    let id: uuid::Uuid = crate::db::scalar_one(&state.pool.get().await?, "INSERT INTO neurovector.customers(provider,subject,name) VALUES($1,$2,$3) ON CONFLICT(provider,subject) DO UPDATE SET name=EXCLUDED.name RETURNING id", &[&(&name), &(subject), &(display_name)]).await?;
    let session = customer_auth::token();
    crate::db::execute(
        &state.pool.get().await?,
        "DELETE FROM neurovector.customer_sessions WHERE expires_at<now()",
        &[],
    )
    .await?;
    crate::db::execute(
        &state.pool.get().await?,
        "INSERT INTO neurovector.customer_sessions VALUES($1,$2,now()+interval '7 days')",
        &[&(customer_auth::digest(&session)), &(id)],
    )
    .await?;
    output.append(
        header::SET_COOKIE,
        customer_auth::cookie("nv_customer", &session, 604800)?,
    );
    output.insert(
        header::CACHE_CONTROL,
        "no-store".parse().map_err(anyhow::Error::from)?,
    );
    Ok((output, store_redirect()?))
}
async fn me(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<(HeaderMap, Json<Customer>), AppError> {
    let user = customer_auth::current(&state, &headers).await?;
    let mut output = HeaderMap::new();
    output.insert(
        header::CACHE_CONTROL,
        "no-store".parse().map_err(anyhow::Error::from)?,
    );
    Ok((output, Json(user)))
}
async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<(HeaderMap, StatusCode), AppError> {
    if let Some(token) = customer_auth::cookie_value(&headers, "nv_customer") {
        crate::db::execute(
            &state.pool.get().await?,
            "DELETE FROM neurovector.customer_sessions WHERE token_hash=$1",
            &[&(customer_auth::digest(&token))],
        )
        .await?;
    }
    let mut output = HeaderMap::new();
    output.insert(
        header::SET_COOKIE,
        customer_auth::cookie("nv_customer", "", 0)?,
    );
    output.insert(
        header::CACHE_CONTROL,
        "no-store".parse().map_err(anyhow::Error::from)?,
    );
    Ok((output, StatusCode::NO_CONTENT))
}

fn store_redirect() -> Result<Redirect, AppError> {
    let origin =
        std::env::var("AUTH_FRONTEND_ORIGIN").unwrap_or_else(|_| "http://localhost:5173".into());
    let url = reqwest::Url::parse(&origin)
        .map_err(|_| AppError::ServiceUnavailable("AUTH_FRONTEND_ORIGIN 오류".into()))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return Err(AppError::ServiceUnavailable(
            "AUTH_FRONTEND_ORIGIN 오류".into(),
        ));
    }
    Ok(Redirect::to(&format!(
        "{}/#store",
        origin.trim_end_matches('/')
    )))
}
