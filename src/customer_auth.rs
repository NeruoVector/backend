use crate::{error::AppError, state::AppState};
use axum::{
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Clone, Serialize)]
pub struct Customer {
    pub id: Uuid,
    pub name: String,
    pub merchant_enabled: bool,
}
crate::db::row!(Customer {
    id,
    name,
    merchant_enabled
});

pub fn token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}
pub fn digest(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}
pub fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|part| {
            let (key, value) = part.trim().split_once('=')?;
            (key == name).then(|| value.to_owned())
        })
}
pub fn cookie(name: &str, value: &str, seconds: u32) -> Result<HeaderValue, AppError> {
    let secure = if std::env::var("AUTH_COOKIE_SECURE")
        .or_else(|_| std::env::var("ADMIN_COOKIE_SECURE"))
        .is_ok_and(|value| value.eq_ignore_ascii_case("true"))
    {
        "; Secure"
    } else {
        ""
    };
    HeaderValue::from_str(&format!(
        "{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={seconds}{secure}"
    ))
    .map_err(|_| AppError::BadRequest("invalid cookie".into()))
}
pub async fn current(state: &AppState, headers: &HeaderMap) -> Result<Customer, AppError> {
    let session = cookie_value(headers, "nv_customer")
        .ok_or_else(|| AppError::Unauthorized("로그인이 필요합니다.".into()))?;
    crate::db::fetch_optional::<Customer>(&state.pool.get().await?, "SELECT c.id,c.name,c.merchant_enabled FROM neurovector.customers c JOIN neurovector.customer_sessions s ON s.customer_id=c.id WHERE s.token_hash=$1 AND s.expires_at>now()", &[&(digest(&session))]).await?
        .ok_or_else(|| AppError::Unauthorized("로그인이 만료되었습니다.".into()))
}
pub async fn require_customer(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, AppError> {
    let customer = current(&state, request.headers()).await?;
    request.extensions_mut().insert(customer);
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

/// SameSite cookies are defense in depth; the API also rejects cross-site writes.
/// Header-less native clients remain supported for explicit non-browser requests.
pub async fn require_same_origin(request: Request, next: Next) -> Response {
    let allowed_origins = configured_origins().collect::<Vec<_>>();
    if request_origin_allowed(request.headers(), &allowed_origins) {
        next.run(request).await
    } else {
        StatusCode::FORBIDDEN.into_response()
    }
}

fn request_origin_allowed(headers: &HeaderMap, allowed_origins: &[String]) -> bool {
    for value in headers.get_all("sec-fetch-site") {
        let Ok(value) = value.to_str() else {
            return false;
        };
        if !matches!(
            value.to_ascii_lowercase().as_str(),
            "same-origin" | "same-site" | "none"
        ) {
            return false;
        }
    }

    let origins = headers.get_all(header::ORIGIN);
    let mut values = origins.iter();
    let Some(origin) = values.next() else {
        return true;
    };
    if values.next().is_some() {
        return false;
    }
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    let Some(origin) = canonical_origin(origin) else {
        return false;
    };
    allowed_origins.iter().any(|allowed| allowed == &origin)
}

fn configured_origins() -> impl Iterator<Item = String> {
    let mut origins = Vec::new();
    origins.push(
        std::env::var("AUTH_PUBLIC_ORIGIN").unwrap_or_else(|_| "http://localhost:5173".into()),
    );
    origins.push(
        std::env::var("AUTH_FRONTEND_ORIGIN").unwrap_or_else(|_| "http://localhost:5173".into()),
    );
    let cors = std::env::var("CORS_ALLOWED_ORIGINS")
        .unwrap_or_else(|_| "http://localhost:5173,http://127.0.0.1:5173".into());
    origins.extend(cors.split(',').map(str::trim).map(ToOwned::to_owned));
    origins
        .into_iter()
        .filter_map(|origin| canonical_origin(&origin))
}

fn canonical_origin(value: &str) -> Option<String> {
    let url = reqwest::Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return None;
    }
    Some(url.origin().ascii_serialization())
}

#[cfg(test)]
mod origin_tests {
    use super::{canonical_origin, request_origin_allowed};
    use axum::http::{HeaderMap, header};

    #[test]
    fn origin_normalization_drops_default_port_and_rejects_paths() {
        assert_eq!(
            canonical_origin("http://LOCALHOST:80/"),
            Some("http://localhost".into())
        );
        assert!(canonical_origin("http://localhost.evil.test").is_some());
        assert!(canonical_origin("http://localhost:5173/evil").is_none());
        assert!(canonical_origin("null").is_none());
    }

    #[test]
    fn origin_guard_keeps_native_clients_and_configured_web_origin() {
        let allowed = vec!["http://localhost:5173".to_string()];
        assert!(request_origin_allowed(&HeaderMap::new(), &allowed));

        let mut headers = HeaderMap::new();
        headers.insert(header::ORIGIN, "http://localhost:5173".parse().unwrap());
        headers.insert("sec-fetch-site", "same-site".parse().unwrap());
        assert!(request_origin_allowed(&headers, &allowed));
    }

    #[test]
    fn origin_guard_rejects_hostile_origin_and_cross_site_fetch_metadata() {
        let allowed = vec!["http://localhost:5173".to_string()];
        let mut headers = HeaderMap::new();
        headers.insert(header::ORIGIN, "https://attacker.example".parse().unwrap());
        assert!(!request_origin_allowed(&headers, &allowed));

        headers.insert(header::ORIGIN, "http://localhost:5173".parse().unwrap());
        headers.insert("sec-fetch-site", "cross-site".parse().unwrap());
        assert!(!request_origin_allowed(&headers, &allowed));
    }
}
