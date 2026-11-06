use super::*;
use config::{ProviderConfig, Settings};
use std::collections::HashMap;

fn configured(provider: Provider) -> ProviderConfig {
    ProviderConfig {
        provider,
        client_id: "client-id".into(),
        client_secret: "server-secret".into(),
        redirect_uri: format!(
            "https://shop.example/api/v1/auth/oauth/{}/callback",
            provider.name()
        ),
    }
}

#[test]
fn authorization_uses_registered_callback_and_bound_state_with_google_pkce() {
    // Given each configured confidential provider and a per-browser flow.
    for provider in Provider::ALL {
        let config = configured(provider);
        // When constructing the provider authorization redirect.
        let url = reqwest::Url::parse(
            &provider::authorization_url(&config, "csrf-state", "verifier").unwrap(),
        )
        .unwrap();
        let query: HashMap<_, _> = url.query_pairs().collect();
        // Then secrets stay server-side and state/callback remain exact.
        assert_eq!(query["state"], "csrf-state");
        assert_eq!(query["redirect_uri"], config.redirect_uri);
        assert_eq!(query["client_id"], "client-id");
        assert!(!url.as_str().contains("server-secret"));
        if provider == Provider::Google {
            assert_eq!(query["code_challenge_method"], "S256");
            assert_eq!(query["code_challenge"], provider::challenge("verifier"));
        }
    }
}

#[test]
fn pkce_matches_rfc7636_s256_vector() {
    assert_eq!(
        provider::challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
}

#[test]
fn callback_requires_same_browser_cookie_and_returned_state() {
    // Given a flow cookie received by the browser that started authorization.
    let token = "a".repeat(64);
    let mut headers = HeaderMap::new();
    headers.insert(
        header::COOKIE,
        format!("{FLOW_COOKIE}={token}").parse().unwrap(),
    );
    let mut callback = Callback {
        code: Some("code".into()),
        state: Some(token.clone()),
        error: None,
    };
    // When an unsolicited or mismatched callback is presented.
    assert!(bound_state(&HeaderMap::new(), &callback).is_none());
    callback.state = Some("b".repeat(64));
    assert!(bound_state(&headers, &callback).is_none());
    // Then only the exact same-browser state can reach server-side redemption.
    callback.state = Some(token.clone());
    assert_eq!(bound_state(&headers, &callback), Some(token));
}

#[test]
fn flow_cookie_is_http_only_lax_for_provider_callback_and_expires() {
    let headers = flow_headers("token", true, 600).unwrap();
    let cookie = headers[header::SET_COOKIE].to_str().unwrap();
    assert!(cookie.contains("HttpOnly; SameSite=Lax"));
    assert!(cookie.contains("Path=/api/v1/auth/oauth"));
    assert!(cookie.ends_with("; Secure"));
    assert!(
        flow_headers("", true, 0).unwrap()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
}

#[test]
fn config_allows_disabled_templates_and_rejects_partial_or_unsafe_callbacks() {
    // Given a template with callback hints but no provider credentials.
    let mut vars = HashMap::from([(
        "GOOGLE_REDIRECT_URI".to_owned(),
        "http://localhost:5173/api/v1/auth/oauth/google/callback".to_owned(),
    )]);
    let load = |vars: &HashMap<String, String>| Settings::from_lookup(|key| vars.get(key).cloned());
    assert!(load(&vars).unwrap().providers.is_empty());
    // When enabling a provider with incomplete or unsafe settings.
    vars.insert("GOOGLE_CLIENT_ID".into(), "id".into());
    assert!(load(&vars).is_err());
    vars.insert("GOOGLE_CLIENT_SECRET".into(), "secret".into());
    assert!(load(&vars).is_ok());
    for url in [
        "https://evil.example/api/v1/auth/oauth/google/callback",
        "http://public.example/api/v1/auth/oauth/google/callback",
        "http://localhost:5173/wrong",
        "http://localhost:5173/api/v1/auth/oauth/google/callback?return_to=evil",
    ] {
        vars.insert("GOOGLE_REDIRECT_URI".into(), url.into());
        assert!(load(&vars).is_err());
    }
}

#[tokio::test]
async fn exchanges_codes_and_reads_stable_identity_over_real_http_for_every_provider() {
    use axum::{extract::Form, routing::post};
    // Given an ephemeral HTTP provider boundary, without touching a real account.
    for selected in Provider::ALL {
        let token = post(
            move |Form(form): Form<HashMap<String, String>>| async move {
                assert_eq!(form["grant_type"], "authorization_code");
                assert_eq!(form["code"], "one-use-code");
                assert_eq!(form["client_secret"], "server-secret");
                assert_eq!(form["redirect_uri"], configured(selected).redirect_uri);
                match selected {
                    Provider::Google => assert_eq!(form["code_verifier"], "verifier"),
                    Provider::Naver => assert_eq!(form["state"], "csrf-state"),
                    Provider::Kakao => {}
                }
                Json(serde_json::json!({"access_token":"access"}))
            },
        );
        let profile = get(move |headers: HeaderMap| async move {
            assert_eq!(headers[header::AUTHORIZATION], "Bearer access");
            Json(match selected {
                Provider::Google => serde_json::json!({"sub":"stable-sub", "name":"테스트"}),
                Provider::Kakao => {
                    serde_json::json!({"id":123, "properties":{"nickname":"테스트"}})
                }
                Provider::Naver => {
                    serde_json::json!({"resultcode":"00", "response":{"id":"stable-sub", "name":"테스트"}})
                }
            })
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/token", token)
            .route("/profile", profile);
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let config = configured(selected);
        // When the backend redeems the authorization code and fetches the profile.
        let result = provider::identity_at(
            provider::Exchange {
                config: &config,
                code: "one-use-code",
                state: "csrf-state",
                verifier: "verifier",
            },
            provider::Endpoints {
                token: &format!("{base}/token"),
                profile: &format!("{base}/profile"),
            },
        )
        .await;
        server.abort();
        // Then provider-stable subject, rather than email/phone, defines the account.
        let identity = result.unwrap();
        assert_eq!(identity.name, "테스트");
        assert_eq!(
            identity.subject,
            if selected == Provider::Kakao {
                "123"
            } else {
                "stable-sub"
            }
        );
    }
}
