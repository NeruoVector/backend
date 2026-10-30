use crate::error::AppError;
use serde::Deserialize;

pub struct Provider {
    pub name: String,
    pub client_id: String,
    pub secret: String,
    pub authorize: &'static str,
    pub token_url: &'static str,
    pub profile_url: &'static str,
    pub scope: &'static str,
}
impl Provider {
    pub fn load(name: &str) -> Result<Self, AppError> {
        let (authorize, token_url, profile_url, scope) = match name {
            "google" => (
                "https://accounts.google.com/o/oauth2/v2/auth",
                "https://oauth2.googleapis.com/token",
                "https://openidconnect.googleapis.com/v1/userinfo",
                "openid profile",
            ),
            "kakao" => (
                "https://kauth.kakao.com/oauth/authorize",
                "https://kauth.kakao.com/oauth/token",
                "https://kapi.kakao.com/v2/user/me",
                "",
            ),
            "naver" => (
                "https://nid.naver.com/oauth2.0/authorize",
                "https://nid.naver.com/oauth2.0/token",
                "https://openapi.naver.com/v1/nid/me",
                "",
            ),
            _ => return Err(AppError::NotFound),
        };
        let prefix = name.to_uppercase();
        let client_id = std::env::var(format!("{prefix}_CLIENT_ID")).unwrap_or_default();
        let secret = std::env::var(format!("{prefix}_CLIENT_SECRET")).unwrap_or_default();
        if client_id.is_empty() || secret.is_empty() {
            return Err(AppError::ServiceUnavailable(format!(
                "{name} 로그인 설정이 필요합니다."
            )));
        }
        Ok(Self {
            name: name.into(),
            client_id,
            secret,
            authorize,
            token_url,
            profile_url,
            scope,
        })
    }
    pub fn redirect_uri(&self) -> Result<String, AppError> {
        let base = std::env::var("AUTH_PUBLIC_ORIGIN").map_err(|_| {
            AppError::ServiceUnavailable("AUTH_PUBLIC_ORIGIN 설정이 필요합니다.".into())
        })?;
        let url = reqwest::Url::parse(&base)
            .map_err(|_| AppError::ServiceUnavailable("AUTH_PUBLIC_ORIGIN 오류".into()))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !matches!(url.path(), "" | "/")
        {
            return Err(AppError::ServiceUnavailable(
                "AUTH_PUBLIC_ORIGIN 오류".into(),
            ));
        }
        Ok(format!(
            "{}/api/v1/auth/{}/callback",
            base.trim_end_matches('/'),
            self.name
        ))
    }
}
#[derive(Deserialize)]
pub struct AccessToken {
    pub access_token: String,
}
#[derive(Deserialize)]
struct GoogleProfile {
    sub: String,
    name: Option<String>,
}
#[derive(Deserialize)]
struct KakaoProperties {
    nickname: Option<String>,
}
#[derive(Deserialize)]
struct KakaoProfile {
    id: u64,
    properties: Option<KakaoProperties>,
}
#[derive(Deserialize)]
struct NaverProfile {
    response: NaverUser,
}
#[derive(Deserialize)]
struct NaverUser {
    id: String,
    name: Option<String>,
    nickname: Option<String>,
}
pub async fn identity(
    provider: &Provider,
    response: reqwest::Response,
) -> Result<(String, String), AppError> {
    let invalid = |_| AppError::Unauthorized("소셜 프로필 조회 실패".into());
    let result = match provider.name.as_str() {
        "google" => {
            let p: GoogleProfile = response.json().await.map_err(invalid)?;
            (p.sub, p.name)
        }
        "kakao" => {
            let p: KakaoProfile = response.json().await.map_err(invalid)?;
            (p.id.to_string(), p.properties.and_then(|v| v.nickname))
        }
        "naver" => {
            let p: NaverProfile = response.json().await.map_err(invalid)?;
            (p.response.id, p.response.name.or(p.response.nickname))
        }
        _ => return Err(AppError::NotFound),
    };
    if result.0.is_empty() {
        return Err(AppError::Unauthorized("소셜 사용자 ID 없음".into()));
    }
    Ok((result.0, result.1.unwrap_or_else(|| "회원".into())))
}
