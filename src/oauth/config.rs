use reqwest::Url;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Kakao,
    Naver,
    Google,
}

impl Provider {
    pub const ALL: [Self; 3] = [Self::Kakao, Self::Naver, Self::Google];
    pub fn name(self) -> &'static str {
        match self {
            Self::Kakao => "kakao",
            Self::Naver => "naver",
            Self::Google => "google",
        }
    }
    pub fn authorization_endpoint(self) -> &'static str {
        match self {
            Self::Kakao => "https://kauth.kakao.com/oauth/authorize",
            Self::Naver => "https://nid.naver.com/oauth2.0/authorize",
            Self::Google => "https://accounts.google.com/o/oauth2/v2/auth",
        }
    }
    pub fn token_endpoint(self) -> &'static str {
        match self {
            Self::Kakao => "https://kauth.kakao.com/oauth/token",
            Self::Naver => "https://nid.naver.com/oauth2.0/token",
            Self::Google => "https://oauth2.googleapis.com/token",
        }
    }
    pub fn profile_endpoint(self) -> &'static str {
        match self {
            Self::Kakao => "https://kapi.kakao.com/v2/user/me",
            Self::Naver => "https://openapi.naver.com/v1/nid/me",
            Self::Google => "https://openidconnect.googleapis.com/v1/userinfo",
        }
    }
}

#[derive(Clone)]
pub struct ProviderConfig {
    pub provider: Provider,
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
}

#[derive(Clone)]
pub struct Settings {
    pub frontend_url: String,
    pub providers: Vec<ProviderConfig>,
}

impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthSettings")
            .field(
                "enabled",
                &self
                    .providers
                    .iter()
                    .map(|p| p.provider)
                    .collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

impl Settings {
    #[cfg(test)]
    pub fn disabled() -> Self {
        Self {
            frontend_url: "http://localhost:5173/".into(),
            providers: Vec::new(),
        }
    }
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_lookup(|key| std::env::var(key).ok().filter(|s| !s.trim().is_empty()))
    }
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let frontend =
            lookup("OAUTH_FRONTEND_URL").unwrap_or_else(|| "http://localhost:5173".into());
        let frontend_url = checked_url(&frontend, "OAUTH_FRONTEND_URL")?;
        anyhow::ensure!(
            frontend_url.path() == "/",
            "OAUTH_FRONTEND_URL must be a site origin"
        );
        let mut providers = Vec::new();
        for provider in Provider::ALL {
            let prefix = provider.name().to_ascii_uppercase();
            let id = lookup(&format!("{prefix}_CLIENT_ID"));
            let secret = lookup(&format!("{prefix}_CLIENT_SECRET"));
            let redirect = lookup(&format!("{prefix}_REDIRECT_URI"));
            if id.is_none() && secret.is_none() {
                continue;
            }
            let client_id = id.ok_or_else(|| anyhow::anyhow!("{prefix}_CLIENT_ID is required"))?;
            let client_secret = secret.unwrap_or_default();
            anyhow::ensure!(
                provider == Provider::Kakao || !client_secret.is_empty(),
                "{prefix}_CLIENT_SECRET is required"
            );
            let redirect_uri =
                redirect.ok_or_else(|| anyhow::anyhow!("{prefix}_REDIRECT_URI is required"))?;
            let callback = checked_url(&redirect_uri, &format!("{prefix}_REDIRECT_URI"))?;
            anyhow::ensure!(
                callback.path() == format!("/api/v1/auth/oauth/{}/callback", provider.name()),
                "{prefix}_REDIRECT_URI has an incorrect callback path"
            );
            anyhow::ensure!(
                callback.host_str() == frontend_url.host_str()
                    && callback.scheme() == frontend_url.scheme(),
                "OAuth frontend and callback must share a scheme and hostname for session cookies"
            );
            providers.push(ProviderConfig {
                provider,
                client_id,
                client_secret,
                redirect_uri,
            });
        }
        Ok(Self {
            frontend_url: frontend_url.to_string(),
            providers,
        })
    }
    pub fn provider(&self, provider: Provider) -> Option<&ProviderConfig> {
        self.providers
            .iter()
            .find(|config| config.provider == provider)
    }
    pub fn secure_cookies(&self) -> bool {
        self.frontend_url.starts_with("https://")
    }
    pub fn return_url(&self, error: Option<&str>) -> String {
        match error {
            Some(code) => format!("{}?oauth_error={code}#/login", self.frontend_url),
            None => format!("{}#/home", self.frontend_url),
        }
    }
}

fn checked_url(value: &str, key: &str) -> anyhow::Result<Url> {
    let url = Url::parse(value).map_err(|_| anyhow::anyhow!("{key} must be an absolute URL"))?;
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    anyhow::ensure!(
        url.scheme() == "https" || (url.scheme() == "http" && loopback),
        "{key} must use HTTPS (HTTP is allowed only on loopback)"
    );
    anyhow::ensure!(
        url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "{key} cannot contain credentials, query or fragment"
    );
    Ok(url)
}
