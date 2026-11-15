use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::config::{Provider, ProviderConfig};

pub struct Identity {
    pub subject: String,
    pub name: String,
}

#[derive(Deserialize)]
struct Token {
    access_token: String,
}
#[derive(Deserialize)]
struct GoogleProfile {
    sub: String,
    name: Option<String>,
}
#[derive(Deserialize)]
struct KakaoProfile {
    id: u64,
    properties: Option<KakaoProperties>,
}
#[derive(Deserialize)]
struct KakaoProperties {
    nickname: Option<String>,
}
#[derive(Deserialize)]
struct NaverEnvelope {
    resultcode: String,
    response: NaverProfile,
}
#[derive(Deserialize)]
struct NaverProfile {
    id: String,
    name: Option<String>,
    nickname: Option<String>,
}

pub fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

pub fn authorization_url(
    config: &ProviderConfig,
    state: &str,
    verifier: &str,
) -> anyhow::Result<String> {
    let mut url = reqwest::Url::parse(config.provider.authorization_endpoint())?;
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("response_type", "code")
            .append_pair("client_id", &config.client_id)
            .append_pair("redirect_uri", &config.redirect_uri)
            .append_pair("state", state);
        match config.provider {
            Provider::Google => {
                query
                    .append_pair("scope", "openid profile")
                    .append_pair("code_challenge_method", "S256")
                    .append_pair("code_challenge", &challenge(verifier));
            }
            Provider::Kakao | Provider::Naver => {}
        }
    }
    Ok(url.into())
}

pub struct Exchange<'a> {
    pub config: &'a ProviderConfig,
    pub code: &'a str,
    pub state: &'a str,
    pub verifier: &'a str,
}

pub struct Endpoints<'a> {
    pub token: &'a str,
    pub profile: &'a str,
}

pub async fn identity(request: Exchange<'_>) -> anyhow::Result<Identity> {
    let provider = request.config.provider;
    identity_at(
        request,
        Endpoints {
            token: provider.token_endpoint(),
            profile: provider.profile_endpoint(),
        },
    )
    .await
}

pub(super) async fn identity_at(
    request: Exchange<'_>,
    endpoints: Endpoints<'_>,
) -> anyhow::Result<Identity> {
    let Exchange {
        config,
        code,
        state,
        verifier,
    } = request;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut form = vec![
        ("grant_type", "authorization_code"),
        ("client_id", config.client_id.as_str()),
        ("redirect_uri", config.redirect_uri.as_str()),
        ("code", code),
    ];
    if !config.client_secret.is_empty() {
        form.push(("client_secret", &config.client_secret));
    }
    match config.provider {
        Provider::Google => form.push(("code_verifier", verifier)),
        Provider::Naver => form.push(("state", state)),
        Provider::Kakao => {}
    }
    let token = client
        .post(endpoints.token)
        .form(&form)
        .send()
        .await?
        .error_for_status()?
        .json::<Token>()
        .await?;
    anyhow::ensure!(
        !token.access_token.is_empty(),
        "Provider returned no access token"
    );
    let response = client
        .get(endpoints.profile)
        .bearer_auth(token.access_token)
        .send()
        .await?
        .error_for_status()?;
    let identity = match config.provider {
        Provider::Google => {
            let user = response.json::<GoogleProfile>().await?;
            Identity {
                subject: user.sub,
                name: user.name.unwrap_or_default(),
            }
        }
        Provider::Kakao => {
            let user = response.json::<KakaoProfile>().await?;
            anyhow::ensure!(user.id > 0, "Invalid Kakao subject");
            Identity {
                subject: user.id.to_string(),
                name: user.properties.and_then(|p| p.nickname).unwrap_or_default(),
            }
        }
        Provider::Naver => {
            let user = response.json::<NaverEnvelope>().await?;
            anyhow::ensure!(user.resultcode == "00", "Naver profile failed");
            Identity {
                subject: user.response.id,
                name: user
                    .response
                    .name
                    .or(user.response.nickname)
                    .unwrap_or_default(),
            }
        }
    };
    anyhow::ensure!(
        !identity.subject.is_empty() && identity.subject.len() <= 255,
        "Invalid provider subject"
    );
    Ok(identity)
}
