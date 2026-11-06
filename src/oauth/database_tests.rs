use super::*;
use provider::Identity;

async fn test_state() -> AppState {
    let url = std::env::var("TEST_DATABASE_URL").expect("isolated TEST_DATABASE_URL");
    crate::state::test_state(
        sqlx::PgPool::connect(&url)
            .await
            .expect("isolated database"),
    )
}

fn request_cookie(headers: HeaderMap) -> HeaderMap {
    let mut request = HeaderMap::new();
    let cookie = headers[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    request.insert(header::COOKIE, cookie.parse().unwrap());
    request
}

#[tokio::test]
#[ignore = "requires an isolated migrated TEST_DATABASE_URL"]
async fn stable_provider_identity_reuses_session_account_without_linking_other_provider() {
    // Given a provider identity first seen by this application.
    let state = test_state().await;
    let subject = Uuid::new_v4().to_string();
    let identity = || Identity {
        subject: subject.clone(),
        name: "소셜 테스트".into(),
    };
    // When signing in twice and presenting the resulting real session cookies.
    let first = accounts::sign_in(&state, Provider::Google, identity())
        .await
        .unwrap();
    let first = crate::customer_auth::current(&state, &request_cookie(first))
        .await
        .unwrap()
        .unwrap();
    let next = accounts::sign_in(&state, Provider::Google, identity())
        .await
        .unwrap();
    let next = crate::customer_auth::current(&state, &request_cookie(next))
        .await
        .unwrap()
        .unwrap();
    let other = accounts::sign_in(&state, Provider::Naver, identity())
        .await
        .unwrap();
    let other = crate::customer_auth::current(&state, &request_cookie(other))
        .await
        .unwrap()
        .unwrap();
    // Then identity is stable per provider; missing phone is not invented or merged.
    assert_eq!(first.id, next.id);
    assert_ne!(first.id, other.id);
    assert_eq!(first.name, "소셜 테스트");
    assert_eq!(first.phone, "");
    assert_eq!(other.phone, "");
    sqlx::query("DELETE FROM neurovector.customer_users WHERE id=$1 OR id=$2")
        .bind(first.id)
        .bind(other.id)
        .execute(&state.pool)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires an isolated migrated TEST_DATABASE_URL"]
async fn oauth_state_is_provider_bound_expires_and_is_consumed_exactly_once() {
    // Given a ten-minute state stored only by its hash.
    let state = test_state().await;
    let token = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO neurovector.customer_oauth_flows(state_hash,provider,verifier,expires_at) VALUES($1,'google','verifier',now()+interval '10 minutes')")
        .bind(hash_token(&token)).execute(&state.pool).await.unwrap();
    // When a different provider, a replay, or expired state attempts redemption.
    assert_eq!(
        accounts::consume_flow(&state, Provider::Naver, &token)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        accounts::consume_flow(&state, Provider::Google, &token)
            .await
            .unwrap(),
        Some("verifier".into())
    );
    assert_eq!(
        accounts::consume_flow(&state, Provider::Google, &token)
            .await
            .unwrap(),
        None
    );
    sqlx::query("INSERT INTO neurovector.customer_oauth_flows(state_hash,provider,verifier,expires_at) VALUES($1,'google','verifier',now()-interval '1 second')")
        .bind(hash_token(&token)).execute(&state.pool).await.unwrap();
    // Then no stale or already-consumed state can authorize a new session.
    assert_eq!(
        accounts::consume_flow(&state, Provider::Google, &token)
            .await
            .unwrap(),
        None
    );
}

#[tokio::test]
#[ignore = "requires an isolated migrated TEST_DATABASE_URL"]
async fn real_customer_phone_numbers_remain_unique_after_oauth_migration() {
    // Given an existing ordinary customer with a real contact.
    let state = test_state().await;
    let id = Uuid::new_v4();
    let phone = format!("010{:08}", id.as_u128() % 100_000_000);
    sqlx::query("INSERT INTO neurovector.customer_users(id,username,password_hash,name,phone) VALUES($1,$2,'test-hash','테스트',$3)")
        .bind(id).bind(id.to_string()).bind(&phone).execute(&state.pool).await.unwrap();
    // When another user attempts to reuse that nonempty phone.
    let duplicate = sqlx::query("INSERT INTO neurovector.customer_users(username,password_hash,name,phone) VALUES($1,'test-hash','테스트',$2)")
        .bind(Uuid::new_v4().to_string()).bind(phone).execute(&state.pool).await;
    // Then the original registration uniqueness contract is retained.
    assert!(
        duplicate
            .unwrap_err()
            .as_database_error()
            .unwrap()
            .is_unique_violation()
    );
    sqlx::query("DELETE FROM neurovector.customer_users WHERE id=$1")
        .bind(id)
        .execute(&state.pool)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires an isolated migrated TEST_DATABASE_URL"]
async fn http_start_and_cancel_callback_bind_cookie_consume_state_and_hide_secrets() {
    // Given the real OAuth router and an isolated database.
    let mut state = test_state().await;
    state.oauth.providers.push(config::ProviderConfig {
        provider: Provider::Google,
        client_id: "public-client".into(),
        client_secret: "must-stay-on-server".into(),
        redirect_uri: "http://localhost/api/v1/auth/oauth/google/callback".into(),
    });
    let app = Router::new().nest("/api/v1", router()).with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    // When the browser starts a real state-bound flow, then cancels it.
    let available = client
        .get(format!("{base}/api/v1/auth/oauth/providers"))
        .send()
        .await
        .unwrap();
    assert_eq!(available.headers()[header::CACHE_CONTROL], "no-store");
    let public = available.text().await.unwrap();
    assert!(public.contains("google"));
    assert!(!public.contains("must-stay-on-server"));
    let start = client
        .get(format!("{base}/api/v1/auth/oauth/google/start"))
        .send()
        .await
        .unwrap();
    assert_eq!(start.status(), reqwest::StatusCode::SEE_OTHER);
    let location =
        reqwest::Url::parse(start.headers()[header::LOCATION].to_str().unwrap()).unwrap();
    assert_eq!(location.host_str(), Some("accounts.google.com"));
    let state = location
        .query_pairs()
        .find(|(key, _)| key == "state")
        .unwrap()
        .1
        .into_owned();
    let cookie = start.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let callback =
        format!("{base}/api/v1/auth/oauth/google/callback?state={state}&error=access_denied");
    let unsolicited = client.get(&callback).send().await.unwrap();
    assert!(
        unsolicited.headers()[header::LOCATION]
            .to_str()
            .unwrap()
            .contains("oauth_error=invalid_state")
    );
    let cancelled = client
        .get(&callback)
        .header(header::COOKIE, cookie)
        .send()
        .await
        .unwrap();
    // Then missing cookies fail, cancellation is local, and replay is rejected.
    assert!(
        cancelled.headers()[header::LOCATION]
            .to_str()
            .unwrap()
            .contains("oauth_error=cancelled")
    );
    assert!(
        !cancelled
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .any(|v| v.to_str().unwrap().starts_with("nv_customer_session="))
    );
    let replay = client
        .get(&callback)
        .header(header::COOKIE, cookie)
        .send()
        .await
        .unwrap();
    assert!(
        replay.headers()[header::LOCATION]
            .to_str()
            .unwrap()
            .contains("oauth_error=invalid_state")
    );
    server.abort();
}
