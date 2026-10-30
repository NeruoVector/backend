use crate::db::PgPool;
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

#[derive(Debug)]
struct AdminCredential {
    user_id: Uuid,
    username: String,
    password_hash: String,
}
crate::db::row!(AdminCredential {
    user_id,
    username,
    password_hash
});

#[derive(Debug)]
struct SessionRow {
    session_id: Uuid,
    username: String,
    expires_at: DateTime<Utc>,
}
crate::db::row!(SessionRow {
    session_id,
    username,
    expires_at
});

/// Ensure the configured administrator exists, rotating an established account only
/// when its configured password changes. A concurrent creator never rotates the winner.
pub async fn bootstrap_admin(pool: &PgPool, username: &str, password: &str) -> anyhow::Result<()> {
    let username = username.trim();
    if username.is_empty() {
        anyhow::bail!("ADMIN_USERNAME must not be empty");
    }
    if password.is_empty() {
        anyhow::bail!("ADMIN_PASSWORD must not be empty");
    }

    let existing = crate::db::scalar_optional::<Uuid>(
        &pool.get().await?,
        "SELECT id FROM neurovector.admin_users WHERE username = $1",
        &[&username],
    )
    .await?;
    if existing.is_none() {
        let password_hash = hash_password_async(password.to_owned()).await?;
        let inserted = crate::db::execute(
            &pool.get().await?,
            "INSERT INTO neurovector.admin_users (username, password_hash) VALUES ($1, $2) ON CONFLICT (username) DO NOTHING",
            &[&username, &password_hash],
        )
        .await?;
        if inserted == 1 {
            tracing::info!(username, "admin account created");
        }
        return Ok(());
    }

    let mut connection = pool.get().await?;
    let tx = connection.transaction().await?;
    let (user_id, password_hash) = crate::db::fetch_one::<(Uuid, String)>(
        &tx,
        "SELECT id, password_hash FROM neurovector.admin_users WHERE username = $1 FOR UPDATE",
        &[&username],
    )
    .await?;
    if !verify_password_async(password.to_owned(), password_hash).await? {
        let next_hash = hash_password_async(password.to_owned()).await?;
        crate::db::execute(&tx, "UPDATE neurovector.admin_users SET password_hash = $2, updated_at = now() WHERE id = $1", &[&user_id, &next_hash]).await?;
        crate::db::execute(
            &tx,
            "DELETE FROM neurovector.admin_sessions WHERE user_id = $1",
            &[&user_id],
        )
        .await?;
        tracing::info!(
            username,
            "admin password updated; existing sessions revoked"
        );
    }
    tx.commit().await?;
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

    let credential = crate::db::fetch_optional::<AdminCredential>(
        &state.pool.get().await?,
        r#"
        SELECT id AS user_id, username, password_hash
        FROM neurovector.admin_users
        WHERE username = $1
        "#,
        &[&(username)],
    )
    .await?;

    let Some(credential) = credential else {
        tracing::warn!(username, "admin login failed");
        return Err(AppError::Unauthorized(
            "아이디 또는 비밀번호가 올바르지 않습니다.".into(),
        ));
    };

    if !verify_password_async(body.password.clone(), credential.password_hash.clone()).await? {
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
        crate::db::execute(
            &state.pool.get().await?,
            "DELETE FROM neurovector.admin_sessions WHERE session_hash = $1",
            &[&(hash_token(&token))],
        )
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

    let session = crate::db::fetch_optional::<SessionRow>(
        &state.pool.get().await?,
        r#"
        SELECT s.id AS session_id, u.username, s.expires_at
        FROM neurovector.admin_sessions s
        JOIN neurovector.admin_users u ON u.id = s.user_id
        WHERE s.session_hash = $1
          AND s.expires_at > now()
        "#,
        &[&(session_hash)],
    )
    .await?
    .ok_or_else(|| {
        AppError::Unauthorized("관리자 세션이 만료되었거나 유효하지 않습니다.".into())
    })?;

    if let Err(err) = crate::db::execute(
        &state.pool.get().await?,
        "UPDATE neurovector.admin_sessions SET last_seen_at = now() WHERE id = $1",
        &[&(session.session_id)],
    )
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

    let mut connection = state.pool.get().await?;
    let tx = connection.transaction().await?;
    crate::db::execute(
        &tx,
        "DELETE FROM neurovector.admin_sessions WHERE expires_at < now()",
        &[],
    )
    .await?;
    crate::db::execute(&tx, "INSERT INTO neurovector.admin_sessions (session_hash, user_id, expires_at) VALUES ($1, $2, $3)", &[&(hash_token(&token)), &(user_id), &(expires_at)])
    .await?;
    tx.commit().await?;
    drop(connection);

    Ok((token, expires_at))
}

// Argon2 is CPU/memory intensive. Bound concurrent work and hold the permit
// inside the blocking task so request cancellation cannot start unbounded work.
static PASSWORD_WORKERS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

async fn hash_password_async(password: String) -> anyhow::Result<String> {
    let permit = PASSWORD_WORKERS.acquire().await?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        hash_password(&password)
    })
    .await?
}

async fn verify_password_async(password: String, hash: String) -> anyhow::Result<bool> {
    let permit = PASSWORD_WORKERS.acquire().await?;
    Ok(tokio::task::spawn_blocking(move || {
        let _permit = permit;
        verify_password(&password, &hash)
    })
    .await?)
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

    #[tokio::test]
    #[ignore = "requires an isolated TEST_DATABASE_URL with migrations"]
    async fn concurrent_bootstrap_preserves_existing_credentials_and_sessions() -> anyhow::Result<()>
    {
        let pool = crate::db::pool(&std::env::var("TEST_DATABASE_URL")?, 4)?;
        let username = format!("qa-bootstrap-{}", Uuid::new_v4());
        let (first, second) = tokio::join!(
            bootstrap_admin(&pool, &username, "synthetic-password"),
            bootstrap_admin(&pool, &username, "synthetic-password")
        );
        first?;
        second?;
        let (id, before) = crate::db::fetch_one::<(Uuid, String)>(
            &pool.get().await?,
            "SELECT id, password_hash FROM neurovector.admin_users WHERE username=$1",
            &[&username],
        )
        .await?;
        crate::db::execute(&pool.get().await?,
            "INSERT INTO neurovector.admin_sessions(user_id,session_hash,expires_at) VALUES ($1,$2,now()+interval '1 hour')",
            &[&id, &username]).await?;
        bootstrap_admin(&pool, &username, "synthetic-password").await?;
        let after = crate::db::scalar_one::<String>(
            &pool.get().await?,
            "SELECT password_hash FROM neurovector.admin_users WHERE id=$1",
            &[&id],
        )
        .await?;
        let sessions = crate::db::scalar_one::<i64>(
            &pool.get().await?,
            "SELECT count(*) FROM neurovector.admin_sessions WHERE user_id=$1",
            &[&id],
        )
        .await?;
        assert_eq!(before, after);
        assert_eq!(sessions, 1);
        assert!(verify_password("synthetic-password", &after));
        bootstrap_admin(&pool, &username, "new-synthetic-password").await?;
        let rotated = crate::db::scalar_one::<String>(
            &pool.get().await?,
            "SELECT password_hash FROM neurovector.admin_users WHERE id=$1",
            &[&id],
        )
        .await?;
        let sessions = crate::db::scalar_one::<i64>(
            &pool.get().await?,
            "SELECT count(*) FROM neurovector.admin_sessions WHERE user_id=$1",
            &[&id],
        )
        .await?;
        assert!(verify_password("new-synthetic-password", &rotated));
        assert_eq!(sessions, 0);
        crate::db::execute(
            &pool.get().await?,
            "DELETE FROM neurovector.admin_users WHERE id=$1",
            &[&id],
        )
        .await?;
        Ok(())
    }

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
