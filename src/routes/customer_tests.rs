use axum::{Router, http::StatusCode};
use reqwest::{Client, Response};
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

async fn payload(response: Response, status: StatusCode) -> Value {
    let actual = response.status();
    let text = response.text().await.expect("response body");
    assert_eq!(actual, status, "{text}");
    serde_json::from_str(&text).expect("JSON response")
}

async fn register(client: &Client, base: &str, name: &str) -> (String, Value) {
    let response = client
        .post(format!("{base}/users"))
        .json(&json!({"username":name,"password":"test-password-123"}))
        .send()
        .await
        .expect("register");
    let cookie = response
        .headers()
        .get("set-cookie")
        .expect("session cookie")
        .to_str()
        .expect("ASCII")
        .split(';')
        .next()
        .expect("cookie pair")
        .to_owned();
    let user = payload(response, StatusCode::CREATED).await;
    (cookie, user)
}

#[tokio::test]
#[ignore = "requires an isolated empty PostgreSQL database in TEST_CUSTOMER_DATABASE_URL"]
async fn customer_apis_enforce_ownership_and_persist_results_over_http() {
    // Given: an isolated database and the real API router, served over HTTP.
    let url = std::env::var("TEST_CUSTOMER_DATABASE_URL").expect("isolated test DB required");
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .expect("test DB");
    sqlx::raw_sql("CREATE EXTENSION IF NOT EXISTS vector; CREATE EXTENSION IF NOT EXISTS pg_trgm; CREATE EXTENSION IF NOT EXISTS \"uuid-ossp\"; CREATE SCHEMA IF NOT EXISTS neurovector;")
        .execute(&pool).await.expect("database initialization");
    let mut migrations: Vec<_> =
        std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))
            .expect("migration directory")
            .map(|entry| entry.expect("migration entry").path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "sql"))
            .collect();
    migrations.sort();
    let mut previous_version = 0;
    for path in migrations {
        let name = path.file_name().unwrap().to_str().unwrap();
        let version: u32 = name
            .split('_')
            .next()
            .unwrap()
            .parse()
            .expect("migration version");
        assert!(
            version > previous_version,
            "duplicate or unordered migration: {name}"
        );
        previous_version = version;
        let migration = std::fs::read_to_string(&path).expect("trusted repository migration");
        sqlx::raw_sql(sqlx::AssertSqlSafe(migration.as_str()))
            .execute(&pool)
            .await
            .expect("migration");
    }
    sqlx::raw_sql(include_str!("../../migrations/0015_customer_apis.sql"))
        .execute(&pool)
        .await
        .expect("legacy migration remains repeat-safe");
    crate::auth::bootstrap_admin(&pool, "testadmin", "test-admin-password")
        .await
        .expect("admin");
    let item: Uuid = sqlx::query_scalar("INSERT INTO neurovector.inventory_items(sku,name,unit_price,stock_quantity) VALUES($1,'test item',1000,10) RETURNING id")
        .bind(Uuid::new_v4().to_string()).fetch_one(&pool).await.expect("item");
    let state = crate::state::test_state(pool.clone());
    let app = Router::new()
        .nest("/api/v1", super::router(state.clone()))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let base = format!("http://{}/api/v1", listener.local_addr().expect("address"));
    let server =
        tokio::spawn(async move { axum::serve(listener, app).await.expect("HTTP server") });
    let client = Client::new();

    // When: two users register and exercise sessions, reviews, schools and notifications.
    let (cookie, user) = register(&client, &base, "customer-a").await;
    let (other_cookie, _) = register(&client, &base, "customer-b").await;
    let session = payload(
        client
            .get(format!("{base}/users/session"))
            .header("Cookie", &cookie)
            .send()
            .await
            .unwrap(),
        StatusCode::OK,
    )
    .await;
    assert_eq!(session["user_id"], user["user_id"]);
    assert!(session.get("password_hash").is_none());
    payload(
        client
            .post(format!("{base}/users"))
            .json(&json!({"username":"customer-a","password":"test-password-123"}))
            .send()
            .await
            .unwrap(),
        StatusCode::CONFLICT,
    )
    .await;
    payload(
        client
            .post(format!("{base}/users/session"))
            .json(&json!({"username":"customer-a","password":"incorrect"}))
            .send()
            .await
            .unwrap(),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    let login = client
        .post(format!("{base}/users/session"))
        .json(&json!({"username":"customer-a","password":"test-password-123"}))
        .send()
        .await
        .unwrap();
    let second_cookie = login.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    payload(login, StatusCode::OK).await;

    let input = json!({"item_id":item,"rating":5,"content":"배송이 좋았습니다."});
    payload(
        client
            .post(format!("{base}/reviews"))
            .json(&input)
            .send()
            .await
            .unwrap(),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    let review = payload(
        client
            .post(format!("{base}/reviews"))
            .header("Cookie", &cookie)
            .json(&input)
            .send()
            .await
            .unwrap(),
        StatusCode::CREATED,
    )
    .await;
    let id = review["id"].as_str().unwrap();
    payload(
        client
            .post(format!("{base}/reviews"))
            .header("Cookie", &cookie)
            .json(&input)
            .send()
            .await
            .unwrap(),
        StatusCode::CONFLICT,
    )
    .await;
    payload(
        client
            .post(format!("{base}/reviews"))
            .header("Cookie", &other_cookie)
            .json(&json!({"item_id":item,"rating":0,"content":"invalid"}))
            .send()
            .await
            .unwrap(),
        StatusCode::BAD_REQUEST,
    )
    .await;
    payload(
        client
            .post(format!("{base}/reviews"))
            .header("Cookie", &other_cookie)
            .json(&json!({"item_id":Uuid::new_v4(),"rating":3,"content":"missing item"}))
            .send()
            .await
            .unwrap(),
        StatusCode::NOT_FOUND,
    )
    .await;
    payload(
        client
            .put(format!("{base}/reviews/{id}"))
            .header("Cookie", &other_cookie)
            .json(&json!({"rating":1,"content":"hijack"}))
            .send()
            .await
            .unwrap(),
        StatusCode::NOT_FOUND,
    )
    .await;
    payload(
        client
            .delete(format!("{base}/reviews/{id}"))
            .header("Cookie", &other_cookie)
            .send()
            .await
            .unwrap(),
        StatusCode::NOT_FOUND,
    )
    .await;
    let updated = payload(
        client
            .put(format!("{base}/reviews/{id}"))
            .header("Cookie", &cookie)
            .json(&json!({"rating":4,"content":"수정된 리뷰"}))
            .send()
            .await
            .unwrap(),
        StatusCode::OK,
    )
    .await;
    assert_eq!(updated["rating"], 4);
    let reviews = payload(
        client
            .get(format!("{base}/reviews?item_id={item}"))
            .send()
            .await
            .unwrap(),
        StatusCode::OK,
    )
    .await;
    assert_eq!(reviews[0]["content"], "수정된 리뷰");

    let notification =
        json!({"user_id":user["user_id"],"title":"배송 안내","body":"배송이 완료되었습니다."});
    payload(
        client
            .post(format!("{base}/admin/notifications"))
            .header("Cookie", &cookie)
            .json(&notification)
            .send()
            .await
            .unwrap(),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    let admin = client
        .post(format!("{base}/admin/auth/login"))
        .json(&json!({"username":"testadmin","password":"test-admin-password"}))
        .send()
        .await
        .unwrap();
    let admin_cookie = admin.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    payload(admin, StatusCode::OK).await;
    payload(
        client
            .get(format!("{base}/users/session"))
            .header("Cookie", &admin_cookie)
            .send()
            .await
            .unwrap(),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    let sent = payload(
        client
            .post(format!("{base}/admin/notifications"))
            .header("Cookie", &admin_cookie)
            .json(&notification)
            .send()
            .await
            .unwrap(),
        StatusCode::CREATED,
    )
    .await;
    let notice_id = sent["id"].as_str().unwrap();
    let notices = payload(
        client
            .get(format!("{base}/notifications"))
            .header("Cookie", &cookie)
            .send()
            .await
            .unwrap(),
        StatusCode::OK,
    )
    .await;
    assert_eq!(notices[0]["body"], notification["body"]);
    let others = payload(
        client
            .get(format!("{base}/notifications"))
            .header("Cookie", &other_cookie)
            .send()
            .await
            .unwrap(),
        StatusCode::OK,
    )
    .await;
    assert_eq!(others, json!([]));
    payload(
        client
            .post(format!("{base}/notifications/{notice_id}/read"))
            .header("Cookie", &other_cookie)
            .send()
            .await
            .unwrap(),
        StatusCode::NOT_FOUND,
    )
    .await;
    let read = payload(
        client
            .post(format!("{base}/notifications/{notice_id}/read"))
            .header("Cookie", &cookie)
            .send()
            .await
            .unwrap(),
        StatusCode::OK,
    )
    .await;
    assert!(read["read_at"].is_string());
    let reread = payload(
        client
            .post(format!("{base}/notifications/{notice_id}/read"))
            .header("Cookie", &cookie)
            .send()
            .await
            .unwrap(),
        StatusCode::OK,
    )
    .await;
    assert_eq!(read["read_at"], reread["read_at"]);
    payload(
        client
            .post(format!("{base}/admin/notifications"))
            .header("Cookie", &admin_cookie)
            .json(&json!({"user_id":Uuid::new_v4(),"title":"test","body":"missing"}))
            .send()
            .await
            .unwrap(),
        StatusCode::NOT_FOUND,
    )
    .await;

    let school = json!({"name":"테스트학교","region":"테스트지역"});
    payload(
        client
            .put(format!("{base}/admin/schools/TEST-001"))
            .header("Cookie", &cookie)
            .json(&school)
            .send()
            .await
            .unwrap(),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    payload(
        client
            .put(format!("{base}/admin/schools/TEST-001"))
            .header("Cookie", &admin_cookie)
            .json(&school)
            .send()
            .await
            .unwrap(),
        StatusCode::OK,
    )
    .await;
    let schools = payload(
        client
            .get(format!("{base}/schools"))
            .query(&[("q", "테스트학교"), ("region", "테스트지역")])
            .send()
            .await
            .unwrap(),
        StatusCode::OK,
    )
    .await;
    assert_eq!(schools[0]["code"], "TEST-001");
    let school = payload(
        client
            .get(format!("{base}/schools/TEST-001"))
            .send()
            .await
            .unwrap(),
        StatusCode::OK,
    )
    .await;
    assert_eq!(school["name"], "테스트학교");
    payload(
        client
            .get(format!("{base}/schools/unknown"))
            .send()
            .await
            .unwrap(),
        StatusCode::NOT_FOUND,
    )
    .await;
    let no_match = payload(
        client
            .get(format!("{base}/schools?q=%25"))
            .send()
            .await
            .unwrap(),
        StatusCode::OK,
    )
    .await;
    assert_eq!(no_match, json!([]));
    payload(
        client
            .get(format!("{base}/schools?limit=0"))
            .send()
            .await
            .unwrap(),
        StatusCode::BAD_REQUEST,
    )
    .await;
    payload(
        client
            .get(format!("{base}/notifications?offset=-1"))
            .header("Cookie", &cookie)
            .send()
            .await
            .unwrap(),
        StatusCode::BAD_REQUEST,
    )
    .await;

    payload(
        client
            .post(format!("{base}/phone-verifications"))
            .json(&json!({"phone":"01012345678"}))
            .send()
            .await
            .unwrap(),
        StatusCode::NOT_IMPLEMENTED,
    )
    .await;
    payload(
        client
            .post(format!("{base}/phone-verifications/confirm"))
            .json(&json!({"verification_id":Uuid::new_v4(),"code":"123456"}))
            .send()
            .await
            .unwrap(),
        StatusCode::NOT_IMPLEMENTED,
    )
    .await;
    payload(
        client
            .post(format!("{base}/payment-methods"))
            .header("Cookie", &cookie)
            .json(&json!({"provider":"test","billing_token":"test-token"}))
            .send()
            .await
            .unwrap(),
        StatusCode::NOT_IMPLEMENTED,
    )
    .await;
    assert_eq!(
        client
            .delete(format!("{base}/reviews/{id}"))
            .header("Cookie", &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        payload(
            client
                .get(format!("{base}/reviews?item_id={item}"))
                .send()
                .await
                .unwrap(),
            StatusCode::OK
        )
        .await,
        json!([])
    );
    assert_eq!(
        client
            .delete(format!("{base}/users/session"))
            .header("Cookie", &cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    payload(
        client
            .get(format!("{base}/users/session"))
            .header("Cookie", &cookie)
            .send()
            .await
            .unwrap(),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    sqlx::query("UPDATE neurovector.user_sessions SET expires_at=now()-interval '1 second'")
        .execute(&pool)
        .await
        .unwrap();
    payload(
        client
            .get(format!("{base}/users/session"))
            .header("Cookie", &second_cookie)
            .send()
            .await
            .unwrap(),
        StatusCode::UNAUTHORIZED,
    )
    .await;

    // Then: persistence, ownership, expiry and explicit placeholder responses were observed over HTTP.
    server.abort();
    pool.close().await;
}
