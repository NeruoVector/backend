use crate::{
    auth::{cookie_value, hash_password_async, hash_token, verify_password_async},
    error::AppError,
    state::AppState,
};
use axum::{
    Json, Router,
    extract::{Path, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::Next,
    response::Response,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const COOKIE: &str = "nv_customer_session";
const TTL: u64 = 60 * 60 * 24 * 7;
#[derive(Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct Customer {
    pub id: Uuid,
    pub username: String,
    pub name: String,
    pub phone: String,
}
#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Register {
    username: String,
    password: String,
    name: String,
    phone: String,
}
#[derive(Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Login {
    username: String,
    password: String,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/register", post(register))
        .route("/auth/login", post(login))
        .route("/auth/me", get(me))
        .route("/auth/logout", post(logout))
        .route("/auth/orders", get(orders))
        .route("/auth/orders/{id}", get(order))
        .route("/auth/reviews", get(reviews))
}
fn denied() -> AppError {
    AppError::Unauthorized("로그인이 필요하거나 아이디 또는 비밀번호가 올바르지 않습니다.".into())
}
fn username(value: &str) -> bool {
    (4..=32).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
fn password(value: &str) -> bool {
    (8..=128).contains(&value.len())
}
fn response_headers(token: &str, secure: bool, ttl: u64) -> Result<HeaderMap, AppError> {
    let suffix = if secure { "; Secure" } else { "" };
    let mut headers = HeaderMap::new();
    headers.insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&format!(
            "{COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={ttl}{suffix}"
        ))
        .map_err(anyhow::Error::from)?,
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(headers)
}
async fn session(state: &AppState, user: Uuid) -> Result<HeaderMap, AppError> {
    let mut tx = state.pool.begin().await?;
    let headers = session_in_tx(&mut tx, user, state.admin_cookie_secure).await?;
    tx.commit().await?;
    Ok(headers)
}
async fn session_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user: Uuid,
    secure: bool,
) -> Result<HeaderMap, AppError> {
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    sqlx::query("INSERT INTO neurovector.customer_sessions(session_hash,user_id,expires_at) VALUES ($1,$2,now()+interval '7 days')")
        .bind(hash_token(&token)).bind(user).execute(&mut **tx).await?;
    response_headers(&token, secure, TTL)
}
pub async fn current(state: &AppState, headers: &HeaderMap) -> Result<Option<Customer>, AppError> {
    let Some(token) = cookie_value(headers, COOKIE) else {
        return Ok(None);
    };
    Ok(sqlx::query_as::<_,Customer>("SELECT u.id,u.username,u.name,u.phone FROM neurovector.customer_users u JOIN neurovector.customer_sessions s ON s.user_id=u.id WHERE s.session_hash=$1 AND s.expires_at>now()")
 .bind(hash_token(&token)).fetch_optional(&state.pool).await?)
}
#[utoipa::path(post,path="/api/v1/auth/register",tag="customer auth",request_body=Register,responses((status=201,description="회원가입 및 로그인",body=Customer),(status=400,description="입력 오류"),(status=409,description="아이디 또는 휴대전화 중복")))]
pub(crate) async fn register(
    State(state): State<AppState>,
    Json(body): Json<Register>,
) -> Result<(StatusCode, HeaderMap, Json<Customer>), AppError> {
    let username_value = body.username.trim().to_ascii_lowercase();
    let phone: String = body
        .phone
        .chars()
        .filter(|c| *c != '-' && *c != ' ')
        .collect();
    if !username(&username_value)
        || !password(&body.password)
        || !(1..=50).contains(&body.name.trim().chars().count())
        || !(10..=11).contains(&phone.len())
        || !phone.bytes().all(|b| b.is_ascii_digit())
        || !phone.starts_with("01")
    {
        return Err(AppError::BadRequest("아이디 4~32자(영문·숫자·_-), 비밀번호 8~128자, 이름 1~50자와 올바른 휴대전화 번호를 입력해 주세요.".into()));
    }
    let hash = hash_password_async(body.password).await?;
    let mut tx = state.pool.begin().await?;
    let user = sqlx::query_as::<_,Customer>("INSERT INTO neurovector.customer_users(username,password_hash,name,phone) VALUES($1,$2,$3,$4) RETURNING id,username,name,phone")
 .bind(username_value).bind(hash).bind(body.name.trim()).bind(phone).fetch_one(&mut *tx).await.map_err(|error| {
 if error.as_database_error().is_some_and(|e| e.is_unique_violation()) { AppError::Conflict("이미 사용 중인 아이디 또는 휴대전화 번호입니다.".into()) } else { AppError::Database(error) }
 })?;
    let headers = session_in_tx(&mut tx, user.id, state.admin_cookie_secure).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, headers, Json(user)))
}
#[utoipa::path(post,path="/api/v1/auth/login",tag="customer auth",request_body=Login,responses((status=200,description="로그인",body=Customer),(status=401,description="잘못된 자격증명")))]
pub(crate) async fn login(
    State(state): State<AppState>,
    Json(body): Json<Login>,
) -> Result<(HeaderMap, Json<Customer>), AppError> {
    if !username(body.username.trim()) || !password(&body.password) {
        return Err(denied());
    }
    let credential: Option<(Uuid, String)> =
        sqlx::query_as("SELECT id,password_hash FROM neurovector.customer_users WHERE username=$1")
            .bind(body.username.trim().to_ascii_lowercase())
            .fetch_optional(&state.pool)
            .await?;
    let Some((id, hash)) = credential else {
        return Err(denied());
    };
    if !verify_password_async(body.password, hash).await? {
        return Err(denied());
    }
    let user = sqlx::query_as::<_, Customer>(
        "SELECT id,username,name,phone FROM neurovector.customer_users WHERE id=$1",
    )
    .bind(id)
    .fetch_one(&state.pool)
    .await?;
    Ok((session(&state, id).await?, Json(user)))
}
#[utoipa::path(get,path="/api/v1/auth/me",tag="customer auth",responses((status=200,description="현재 고객",body=Customer),(status=401,description="로그인 필요")))]
pub(crate) async fn me(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<(HeaderMap, Json<Customer>), AppError> {
    let user = current(&state, &headers).await?.ok_or_else(denied)?;
    let mut response = HeaderMap::new();
    response.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok((response, Json(user)))
}
#[utoipa::path(post,path="/api/v1/auth/logout",tag="customer auth",responses((status=204,description="세션 폐기")))]
pub(crate) async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<(StatusCode, HeaderMap), AppError> {
    if let Some(token) = cookie_value(&headers, COOKIE) {
        sqlx::query("DELETE FROM neurovector.customer_sessions WHERE session_hash=$1")
            .bind(hash_token(&token))
            .execute(&state.pool)
            .await?;
    }
    Ok((
        StatusCode::NO_CONTENT,
        response_headers("", state.admin_cookie_secure, 0)?,
    ))
}
#[utoipa::path(get,path="/api/v1/auth/orders",tag="customer auth",responses((status=200,description="내 주문 목록",body=serde_json::Value),(status=401,description="로그인 필요")))]
pub(crate) async fn orders(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    let user = current(&state, &headers).await?.ok_or_else(denied)?;
    let items: Vec<serde_json::Value>=sqlx::query_scalar("SELECT to_jsonb(o)-'customer_user_id' FROM neurovector.orders o WHERE customer_user_id=$1 ORDER BY created_at DESC LIMIT 100").bind(user.id).fetch_all(&state.pool).await?;
    let total: i64 =
        sqlx::query_scalar("SELECT count(*) FROM neurovector.orders WHERE customer_user_id=$1")
            .bind(user.id)
            .fetch_one(&state.pool)
            .await?;
    Ok(Json(serde_json::json!({"items":items,"total":total})))
}
#[utoipa::path(get,path="/api/v1/auth/orders/{id}",tag="customer auth",params(("id"=Uuid,Path,description="내 주문 ID")),responses((status=200,description="내 주문 상세",body=crate::routes::orders::OrderDetail),(status=404,description="소유한 주문 없음")))]
pub(crate) async fn order(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<(HeaderMap, Json<crate::routes::orders::OrderDetail>), AppError> {
    let user = current(&state, &headers).await?.ok_or_else(denied)?;
    let owned: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM neurovector.orders WHERE id=$1 AND customer_user_id=$2)",
    )
    .bind(id)
    .bind(user.id)
    .fetch_one(&state.pool)
    .await?;
    if !owned {
        return Err(AppError::NotFound);
    }
    crate::routes::orders::get_order(State(state), Path(id)).await
}
#[utoipa::path(get,path="/api/v1/auth/reviews",tag="customer auth",responses((status=200,description="내 리뷰 목록",body=serde_json::Value),(status=401,description="로그인 필요")))]
pub(crate) async fn reviews(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    let user = current(&state, &headers).await?.ok_or_else(denied)?;
    let items:Vec<serde_json::Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',r.id,'order_id',r.order_id,'rating',r.rating,'content',r.content,'tags',r.tags,'has_photo',r.photo_data IS NOT NULL,'created_at',r.created_at) FROM neurovector.order_reviews r JOIN neurovector.orders o ON o.id=r.order_id WHERE o.customer_user_id=$1 ORDER BY r.created_at DESC LIMIT 100").bind(user.id).fetch_all(&state.pool).await?;
    let (total,average_rating):(i64,Option<f64>)=sqlx::query_as("SELECT count(*),avg(r.rating)::float8 FROM neurovector.order_reviews r JOIN neurovector.orders o ON o.id=r.order_id WHERE o.customer_user_id=$1").bind(user.id).fetch_one(&state.pool).await?;
    Ok(Json(
        serde_json::json!({"items":items,"total":total,"average_rating":average_rating}),
    ))
}

fn same_origin(origin: &str, host: &str, secure: bool) -> bool {
    origin == format!("https://{host}") || (!secure && origin == format!("http://{host}"))
}

/// Authorize before handlers and caches, including payment and review routes.
pub async fn protect(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    if request.method() != Method::GET && request.method() != Method::HEAD {
        if let Some(origin) = request.headers().get(header::ORIGIN) {
            let origin = origin
                .to_str()
                .map_err(|_| AppError::BadRequest("잘못된 Origin입니다.".into()))?;
            let host = request
                .headers()
                .get(header::HOST)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if !same_origin(origin, host, state.admin_cookie_secure) {
                return Err(AppError::BadRequest(
                    "동일한 사이트에서 요청해 주세요.".into(),
                ));
            }
        }
    }
    if request.uri().path().split('/').any(|part| part == "admin") {
        return Ok(next.run(request).await);
    }
    let parts: Vec<_> = request.uri().path().trim_matches('/').split('/').collect();
    let target = parts.windows(2).find_map(|p| match p[0] {
        "orders" => Uuid::parse_str(p[1]).ok().map(|id| (id, false)),
        "payments" => Uuid::parse_str(p[1]).ok().map(|id| (id, true)),
        _ => None,
    });
    if let Some((id, payment)) = target {
        let owner: Option<Option<Uuid>> = if payment {
            sqlx::query_scalar("SELECT o.customer_user_id FROM neurovector.orders o JOIN neurovector.payments p ON p.order_id=o.id WHERE p.id=$1").bind(id).fetch_optional(&state.pool).await?
        } else {
            sqlx::query_scalar("SELECT customer_user_id FROM neurovector.orders WHERE id=$1")
                .bind(id)
                .fetch_optional(&state.pool)
                .await?
        };
        if let Some(Some(owner)) = owner {
            if let Some(token) = cookie_value(request.headers(), crate::auth::SESSION_COOKIE_NAME) {
                let admin: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM neurovector.admin_sessions WHERE session_hash=$1 AND expires_at>now())")
                    .bind(hash_token(&token)).fetch_one(&state.pool).await?;
                if admin {
                    return Ok(next.run(request).await);
                }
            }
            let user = current(&state, request.headers())
                .await?
                .ok_or_else(denied)?;
            if user.id != owner {
                return Err(AppError::NotFound);
            }
        }
    }
    let private = request.uri().path().split('/').any(|part| part == "auth")
        || cookie_value(request.headers(), COOKIE).is_some();
    let mut response = next.run(request).await;
    if private {
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    Ok(response)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn secure_origin_requires_https_and_exact_host_with_port() {
        assert!(same_origin(
            "https://example.test:8443",
            "example.test:8443",
            true
        ));
        assert!(!same_origin("http://example.test", "example.test", true));
        assert!(!same_origin("https://evil.test", "example.test", true));
        assert!(same_origin(
            "http://localhost:8080",
            "localhost:8080",
            false
        ));
        assert!(!same_origin("null", "localhost:8080", false));
    }
    #[test]
    fn rejects_short_or_unsafe_credentials() {
        assert!(!username("ab"));
        assert!(!username("ab cd"));
        assert!(!password("1234"));
        assert!(username("test_user"));
        assert!(password("password123"));
    }
    #[test]
    fn cookie_is_separate_and_protected() {
        let h = response_headers("token", true, TTL).unwrap();
        let value = h[header::SET_COOKIE].to_str().unwrap();
        assert!(value.starts_with("nv_customer_session="));
        assert!(value.contains("HttpOnly; SameSite=Strict"));
        assert!(value.contains("Secure"));
    }
}

#[cfg(test)]
mod database_tests {
    use super::*;
    async fn state() -> Option<AppState> {
        let Ok(url) = std::env::var("TEST_DATABASE_URL") else {
            return None;
        };
        Some(crate::state::test_state(
            sqlx::PgPool::connect(&url).await.expect("test database"),
        ))
    }
    fn input(suffix: &str, phone: &str) -> Register {
        Register {
            username: format!("qa_{suffix}"),
            password: "customer-test-password".into(),
            name: "테스트".into(),
            phone: phone.into(),
        }
    }
    fn cookie(headers: &HeaderMap) -> HeaderMap {
        let mut request = HeaderMap::new();
        let value = headers[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        request.insert(header::COOKIE, HeaderValue::from_str(value).unwrap());
        request
    }
    #[tokio::test]
    async fn legacy_pickup_repair_is_random_and_runs_only_once() {
        let Some(state) = state().await else {
            return;
        };
        let mut tx = state.pool.begin().await.unwrap();
        sqlx::raw_sql("CREATE TEMP TABLE orders (id uuid, order_number text, pickup_code text); INSERT INTO orders SELECT uuid_generate_v4(), 'NV-20260101-ABCDEF12', '1234' FROM generate_series(1,20); INSERT INTO orders VALUES(uuid_generate_v4(),'NV-20260101-0123456789abcdef0123456789abcdef-9876','9876');")
            .execute(&mut *tx).await.unwrap();
        let migration = include_str!("../migrations/0011_randomize_legacy_pickup_codes.sql")
            .replace("neurovector.", "pg_temp.");
        // Trusted embedded migration; only a constant test-schema name is substituted.
        sqlx::raw_sql(sqlx::AssertSqlSafe(migration.as_str()))
            .execute(&mut *tx).await.unwrap();
        let first: Vec<(Uuid, String)> =
            sqlx::query_as("SELECT id,pickup_code FROM pg_temp.orders ORDER BY id")
                .fetch_all(&mut *tx)
                .await
                .unwrap();
        assert!(
            first
                .iter()
                .all(|(_, code)| code.len() == 4 && code.bytes().all(|b| b.is_ascii_digit()))
        );
        let changed: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_temp.orders WHERE order_number='NV-20260101-ABCDEF12' AND pickup_code<>'1234'").fetch_one(&mut *tx).await.unwrap();
        assert!(changed > 0);
        let modern: String = sqlx::query_scalar(
            "SELECT pickup_code FROM pg_temp.orders WHERE order_number LIKE '%-9876'",
        )
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert_eq!(modern, "9876");
        sqlx::raw_sql(sqlx::AssertSqlSafe(migration.as_str()))
            .execute(&mut *tx).await.unwrap();
        let repeated: Vec<(Uuid, String)> =
            sqlx::query_as("SELECT id,pickup_code FROM pg_temp.orders ORDER BY id")
                .fetch_all(&mut *tx)
                .await
                .unwrap();
        assert_eq!(first, repeated);
        tx.rollback().await.unwrap();
    }
    #[tokio::test]
    async fn rejects_wrong_password_and_duplicate_identity() {
        let Some(state) = state().await else {
            return;
        };
        let suffix = Uuid::new_v4().simple().to_string()[..16].to_string();
        let phone = format!("010{:08}", Uuid::new_v4().as_u128() % 100_000_000);
        let (_, _, Json(user)) = register(State(state.clone()), Json(input(&suffix, &phone)))
            .await
            .unwrap();
        let wrong = login(
            State(state.clone()),
            Json(Login {
                username: user.username.clone(),
                password: "wrong-password".into(),
            }),
        )
        .await;
        assert!(matches!(wrong, Err(AppError::Unauthorized(_))));
        let duplicate = register(State(state.clone()), Json(input(&suffix, &phone))).await;
        assert!(matches!(duplicate, Err(AppError::Conflict(_))));
        sqlx::query("DELETE FROM neurovector.customer_users WHERE id=$1")
            .bind(user.id)
            .execute(&state.pool)
            .await
            .unwrap();
    }
    #[tokio::test]
    async fn logout_revokes_persistent_session_and_expired_sessions_fail() {
        let Some(state) = state().await else {
            return;
        };
        let suffix = Uuid::new_v4().simple().to_string()[..16].to_string();
        let phone = format!("010{:08}", Uuid::new_v4().as_u128() % 100_000_000);
        let (_, headers, Json(user)) = register(State(state.clone()), Json(input(&suffix, &phone)))
            .await
            .unwrap();
        let headers = cookie(&headers);
        assert_eq!(
            current(&state, &headers).await.unwrap().unwrap().id,
            user.id
        );
        logout(State(state.clone()), headers.clone()).await.unwrap();
        assert!(current(&state, &headers).await.unwrap().is_none());
        let headers = cookie(&session(&state, user.id).await.unwrap());
        sqlx::query("UPDATE neurovector.customer_sessions SET expires_at=now()-interval '1 second' WHERE user_id=$1").bind(user.id).execute(&state.pool).await.unwrap();
        assert!(current(&state, &headers).await.unwrap().is_none());
        sqlx::query("DELETE FROM neurovector.customer_users WHERE id=$1")
            .bind(user.id)
            .execute(&state.pool)
            .await
            .unwrap();
    }
}
