use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
};
use axum::{
    Extension, Json,
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::Next,
    response::Response,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{error::AppError, state::AppState};

pub const SESSION_COOKIE_NAME: &str = "nv_admin_session";

#[derive(Debug, Clone)]
pub struct CurrentAdmin {
    pub username: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AdminSessionInfo {
    pub username: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, FromRow)]
struct AdminCredential {
    user_id: Uuid,
    username: String,
    password_hash: String,
}

#[derive(Debug, FromRow)]
struct SessionRow {
    session_id: Uuid,
    username: String,
    expires_at: DateTime<Utc>,
}

/// 환경 변수에 설정된 관리자 계정을 보장한다.
///
/// 기존 계정이 있고 비밀번호가 바뀌면 해시를 갱신하고 기존 세션을 모두 폐기한다.
pub async fn bootstrap_admin(pool: &PgPool, username: &str, password: &str) -> anyhow::Result<()> {
    let username = username.trim();
    if username.is_empty() {
        anyhow::bail!("ADMIN_USERNAME must not be empty");
    }
    if password.is_empty() {
        anyhow::bail!("ADMIN_PASSWORD must not be empty");
    }

    let existing = sqlx::query_as::<_, (Uuid, String)>(
        "SELECT id, password_hash FROM neurovector.admin_users WHERE username = $1",
    )
    .bind(username)
    .fetch_optional(pool)
    .await?;

    if let Some((user_id, password_hash)) = existing {
        if verify_password(password, &password_hash) {
            return Ok(());
        }

        let next_hash = hash_password(password)?;
        let mut tx = pool.begin().await?;
        sqlx::query(
            "UPDATE neurovector.admin_users SET password_hash = $2, updated_at = now() WHERE id = $1",
        )
        .bind(user_id)
        .bind(&next_hash)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM neurovector.admin_sessions WHERE user_id = $1")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        tracing::info!(
            username,
            "admin password updated; existing sessions revoked"
        );
    } else {
        let password_hash = hash_password(password)?;
        sqlx::query(
            "INSERT INTO neurovector.admin_users (username, password_hash) VALUES ($1, $2)",
        )
        .bind(username)
        .bind(password_hash)
        .execute(pool)
        .await?;
        tracing::info!(username, "admin account created");
    }

    Ok(())
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/auth/login",
    request_body = LoginRequest,
    responses(
        (status = 200, description = "관리자 로그인 성공", body = AdminSessionInfo),
        (status = 401, description = "아이디 또는 비밀번호 불일치")
    ),
    tag = "admin"
)]
pub(crate) async fn login(
    State(state): State<AppState>,
    Json(body): Json<LoginRequest>,
) -> Result<(HeaderMap, Json<AdminSessionInfo>), AppError> {
    let username = body.username.trim();
    if username.is_empty() || body.password.is_empty() {
        return Err(AppError::Unauthorized(
            "아이디 또는 비밀번호가 올바르지 않습니다.".into(),
        ));
    }

    let credential = sqlx::query_as::<_, AdminCredential>(
        r#"
        SELECT id AS user_id, username, password_hash
        FROM neurovector.admin_users
        WHERE username = $1
        "#,
    )
    .bind(username)
    .fetch_optional(&state.pool)
    .await?;

    let Some(credential) = credential else {
        tracing::warn!(username, "admin login failed");
        return Err(AppError::Unauthorized(
            "아이디 또는 비밀번호가 올바르지 않습니다.".into(),
        ));
    };

    if !verify_password(&body.password, &credential.password_hash) {
        tracing::warn!(username, "admin login failed");
        return Err(AppError::Unauthorized(
            "아이디 또는 비밀번호가 올바르지 않습니다.".into(),
        ));
    }

    let ttl_seconds = state.admin_session_ttl_seconds.clamp(60, 60 * 60 * 24 * 30);
    let (token, expires_at) = create_session(&state, credential.user_id, ttl_seconds).await?;
    tracing::info!(username = %credential.username, "admin login succeeded");

    let mut headers = HeaderMap::new();
    headers.insert(
        header::SET_COOKIE,
        session_cookie(&token, ttl_seconds, state.admin_cookie_secure),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));

    Ok((
        headers,
        Json(AdminSessionInfo {
            username: credential.username,
            expires_at,
        }),
    ))
}

#[utoipa::path(
    post,
    path = "/api/v1/admin/auth/logout",
    responses((status = 204, description = "관리자 로그아웃")),
    tag = "admin"
)]
pub(crate) async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<(StatusCode, HeaderMap), AppError> {
    if let Some(token) = cookie_value(&headers, SESSION_COOKIE_NAME) {
        sqlx::query("DELETE FROM neurovector.admin_sessions WHERE session_hash = $1")
            .bind(hash_token(&token))
            .execute(&state.pool)
            .await?;
    }

    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::SET_COOKIE,
        cleared_session_cookie(state.admin_cookie_secure),
    );
    response_headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));

    Ok((StatusCode::NO_CONTENT, response_headers))
}

#[utoipa::path(
    get,
    path = "/api/v1/admin/auth/me",
    responses(
        (status = 200, description = "현재 관리자 세션", body = AdminSessionInfo),
        (status = 401, description = "로그인 필요")
    ),
    security(("admin_session" = [])),
    tag = "admin"
)]
pub(crate) async fn me(
    Extension(admin): Extension<CurrentAdmin>,
) -> (HeaderMap, Json<AdminSessionInfo>) {
    let mut headers = HeaderMap::new();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    (
        headers,
        Json(AdminSessionInfo {
            username: admin.username,
            expires_at: admin.expires_at,
        }),
    )
}

/// 관리자 세션 쿠키를 검증하고 요청 확장에 `CurrentAdmin` 을 넣는다.
pub async fn require_admin(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, AppError> {
    let token = cookie_value(request.headers(), SESSION_COOKIE_NAME)
        .ok_or_else(|| AppError::Unauthorized("관리자 로그인이 필요합니다.".into()))?;
    let session_hash = hash_token(&token);

    let session = sqlx::query_as::<_, SessionRow>(
        r#"
        SELECT s.id AS session_id, u.username, s.expires_at
        FROM neurovector.admin_sessions s
        JOIN neurovector.admin_users u ON u.id = s.user_id
        WHERE s.session_hash = $1
          AND s.expires_at > now()
        "#,
    )
    .bind(session_hash)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| {
        AppError::Unauthorized("관리자 세션이 만료되었거나 유효하지 않습니다.".into())
    })?;

    if let Err(err) =
        sqlx::query("UPDATE neurovector.admin_sessions SET last_seen_at = now() WHERE id = $1")
            .bind(session.session_id)
            .execute(&state.pool)
            .await
    {
        tracing::warn!(error = %err, "failed to update admin session last_seen_at");
    }

    request.extensions_mut().insert(CurrentAdmin {
        username: session.username,
        expires_at: session.expires_at,
    });

    Ok(next.run(request).await)
}

async fn create_session(
    state: &AppState,
    user_id: Uuid,
    ttl_seconds: u64,
) -> Result<(String, DateTime<Utc>), AppError> {
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let expires_at = Utc::now() + Duration::seconds(ttl_seconds as i64);

    let mut tx = state.pool.begin().await?;
    sqlx::query("DELETE FROM neurovector.admin_sessions WHERE expires_at < now()")
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO neurovector.admin_sessions (session_hash, user_id, expires_at) VALUES ($1, $2, $3)",
    )
    .bind(hash_token(&token))
    .bind(user_id)
    .bind(expires_at)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok((token, expires_at))
}

fn hash_password(password: &str) -> anyhow::Result<String> {
    let salt = SaltString::encode_b64(Uuid::new_v4().as_bytes())
        .map_err(|err| anyhow::anyhow!("failed to generate password salt: {err}"))?;
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|err| anyhow::anyhow!("failed to hash admin password: {err}"))?;
    Ok(hash.to_string())
}

fn verify_password(password: &str, hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    raw.split(';').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        (key == name).then(|| value.to_string())
    })
}

fn session_cookie(token: &str, max_age_seconds: u64, secure: bool) -> HeaderValue {
    let secure_suffix = if secure { "; Secure" } else { "" };
    let value = format!(
        "{SESSION_COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age_seconds}{secure_suffix}"
    );
    HeaderValue::from_str(&value).expect("valid admin session cookie")
}

fn cleared_session_cookie(secure: bool) -> HeaderValue {
    let secure_suffix = if secure { "; Secure" } else { "" };
    let value = format!(
        "{SESSION_COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0; Expires=Thu, 01 Jan 1970 00:00:00 GMT{secure_suffix}"
    );
    HeaderValue::from_str(&value).expect("valid cleared admin session cookie")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_hash_roundtrip() {
        let hash = hash_password("correct horse battery staple").expect("hash password");
        assert!(verify_password("correct horse battery staple", &hash));
        assert!(!verify_password("wrong password", &hash));
    }

    #[test]
    fn extracts_session_cookie_from_cookie_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("theme=dark; nv_admin_session=abc123; lang=ko"),
        );
        assert_eq!(
            cookie_value(&headers, SESSION_COOKIE_NAME),
            Some("abc123".to_string())
        );
    }

    #[test]
    fn session_cookie_is_http_only_and_same_site_strict() {
        let cookie = session_cookie("token", 60, true);
        let value = cookie.to_str().expect("cookie header is ascii");
        assert!(value.contains("HttpOnly"));
        assert!(value.contains("SameSite=Strict"));
        assert!(value.contains("Max-Age=60"));
        assert!(value.contains("Secure"));
    }
}
