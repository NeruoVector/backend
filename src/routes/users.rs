use crate::{auth, error::AppError, state::AppState};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    routing::post,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

pub const COOKIE_NAME: &str = "nv_user_session";
const SESSION_SECONDS: u32 = 86_400;

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

#[derive(Serialize, FromRow, ToSchema)]
pub struct UserSession {
    pub user_id: Uuid,
    pub username: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(FromRow)]
struct CredentialRow {
    id: Uuid,
    password_hash: String,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/users", post(register))
        .route("/users/session", post(login).get(me).delete(logout))
}

pub(crate) async fn current_user(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<UserSession, AppError> {
    let token = auth::cookie_value(headers, COOKIE_NAME)
        .ok_or_else(|| AppError::Unauthorized("사용자 로그인이 필요합니다.".into()))?;
    sqlx::query_as::<_, UserSession>(
        "SELECT u.id AS user_id, u.username, s.expires_at FROM neurovector.user_sessions s JOIN neurovector.users u ON u.id=s.user_id WHERE s.session_hash=$1 AND s.expires_at>now()"
    ).bind(auth::hash_token(&token)).fetch_optional(&state.pool).await?
        .ok_or_else(|| AppError::Unauthorized("사용자 세션이 만료되었거나 유효하지 않습니다.".into()))
}

fn session_headers(token: &str, secure: bool, max_age: u32) -> Result<HeaderMap, AppError> {
    let mut headers = HeaderMap::new();
    let suffix = if secure { "; Secure" } else { "" };
    let cookie = format!(
        "{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}{suffix}"
    );
    headers.insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(|e| AppError::Other(e.into()))?,
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(headers)
}

#[utoipa::path(post, path="/api/v1/users", request_body=Credentials,
    responses((status=201, description="사용자 등록", body=UserSession), (status=400, description="입력 오류"), (status=409, description="중복 사용자")), tag="users")]
pub async fn register(
    State(state): State<AppState>,
    Json(body): Json<Credentials>,
) -> Result<(StatusCode, HeaderMap, Json<UserSession>), AppError> {
    let username = body.username.trim();
    if !(3..=80).contains(&username.chars().count())
        || !(8..=128).contains(&body.password.chars().count())
    {
        return Err(AppError::BadRequest(
            "username은 3~80자, password는 8~128자여야 합니다.".into(),
        ));
    }
    let hash = auth::hash_password_async(body.password).await?;
    let mut tx = state.pool.begin().await?;
    let id = sqlx::query_scalar::<_, Uuid>("INSERT INTO neurovector.users(username,password_hash) VALUES($1,$2) ON CONFLICT(username) DO NOTHING RETURNING id")
        .bind(username).bind(hash).fetch_optional(&mut *tx).await?
        .ok_or_else(|| AppError::Conflict("이미 등록된 사용자입니다.".into()))?;
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let expires_at = sqlx::query_scalar::<_, DateTime<Utc>>("INSERT INTO neurovector.user_sessions(session_hash,user_id,expires_at) VALUES($1,$2,now()+interval '1 day') RETURNING expires_at")
        .bind(auth::hash_token(&token)).bind(id).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        session_headers(&token, state.admin_cookie_secure, SESSION_SECONDS)?,
        Json(UserSession {
            user_id: id,
            username: username.into(),
            expires_at,
        }),
    ))
}

#[utoipa::path(post, path="/api/v1/users/session", request_body=Credentials,
    responses((status=200, description="로그인 및 세션 쿠키 발급", body=UserSession), (status=401, description="인증 실패")), tag="users")]
pub async fn login(
    State(state): State<AppState>,
    Json(body): Json<Credentials>,
) -> Result<(HeaderMap, Json<UserSession>), AppError> {
    if body.username.len() > 320 || body.password.len() > 512 {
        return Err(AppError::Unauthorized(
            "아이디 또는 비밀번호가 올바르지 않습니다.".into(),
        ));
    }
    let row = sqlx::query_as::<_, CredentialRow>(
        "SELECT id,password_hash FROM neurovector.users WHERE username=$1",
    )
    .bind(body.username.trim())
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(|| AppError::Unauthorized("아이디 또는 비밀번호가 올바르지 않습니다.".into()))?;
    if !auth::verify_password_async(body.password, row.password_hash).await? {
        return Err(AppError::Unauthorized(
            "아이디 또는 비밀번호가 올바르지 않습니다.".into(),
        ));
    }
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let expires_at = sqlx::query_scalar::<_, DateTime<Utc>>("INSERT INTO neurovector.user_sessions(session_hash,user_id,expires_at) VALUES($1,$2,now()+interval '1 day') RETURNING expires_at")
        .bind(auth::hash_token(&token)).bind(row.id).fetch_one(&state.pool).await?;
    Ok((
        session_headers(&token, state.admin_cookie_secure, SESSION_SECONDS)?,
        Json(UserSession {
            user_id: row.id,
            username: body.username.trim().into(),
            expires_at,
        }),
    ))
}

#[utoipa::path(get, path="/api/v1/users/session", security(("user_session"=[])),
    responses((status=200, description="현재 사용자 세션", body=UserSession), (status=401, description="로그인 필요")), tag="users")]
pub async fn me(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<(HeaderMap, Json<UserSession>), AppError> {
    let user = current_user(&state, &headers).await?;
    let mut response_headers = HeaderMap::new();
    response_headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok((response_headers, Json(user)))
}

#[utoipa::path(delete, path="/api/v1/users/session", responses((status=204, description="세션 폐기 및 쿠키 삭제")), tag="users")]
pub async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<(StatusCode, HeaderMap), AppError> {
    if let Some(token) = auth::cookie_value(&headers, COOKIE_NAME) {
        sqlx::query("DELETE FROM neurovector.user_sessions WHERE session_hash=$1")
            .bind(auth::hash_token(&token))
            .execute(&state.pool)
            .await?;
    }
    Ok((
        StatusCode::NO_CONTENT,
        session_headers("", state.admin_cookie_secure, 0)?,
    ))
}
