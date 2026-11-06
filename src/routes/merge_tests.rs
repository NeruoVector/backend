use axum::{Router, http::StatusCode};
use reqwest::{Client, Response};
use serde_json::{Value, json};
use uuid::Uuid;

async fn payload(response: Response, expected: StatusCode) -> Value {
    let status = response.status();
    let text = response.text().await.unwrap();
    assert_eq!(status, expected, "{text}");
    serde_json::from_str(&text).unwrap()
}

#[tokio::test]
#[ignore = "requires an isolated migrated TEST_DATABASE_URL"]
async fn merged_router_preserves_both_review_contracts_and_session_boundaries() {
    // Given: the complete merged router and distinct customer/user accounts.
    let pool = sqlx::PgPool::connect(&std::env::var("TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    let state = crate::state::test_state(pool.clone());
    let app = Router::new()
        .nest("/api/v1", super::router(state.clone()))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/api/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = Client::new();
    let seed = Uuid::new_v4();
    let suffix = seed.simple().to_string();
    let phone = format!("010{:08}", seed.as_u128() % 100_000_000);
    let response = client.post(format!("{base}/auth/register"))
        .json(&json!({"username":format!("merge_{}", &suffix[..12]),"password":"merge-password-123","name":"Merge QA","phone":phone}))
        .send().await.unwrap();
    let customer_cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let customer = payload(response, StatusCode::CREATED).await;
    let response = client
        .post(format!("{base}/users"))
        .json(&json!({"username":format!("legacy_{suffix}"),"password":"merge-password-123"}))
        .send()
        .await
        .unwrap();
    let user_cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    payload(response, StatusCode::CREATED).await;
    let item: Uuid = sqlx::query_scalar("INSERT INTO neurovector.inventory_items(sku,name,unit_price,stock_quantity) VALUES($1,'merge item',1000,10) RETURNING id")
        .bind(&suffix).fetch_one(&pool).await.unwrap();
    let owner: Uuid = customer["id"].as_str().unwrap().parse().unwrap();
    let order: Uuid = sqlx::query_scalar("INSERT INTO neurovector.orders(order_number,customer_name,customer_phone,delivery_address,status,pickup_code,customer_user_id) VALUES($1,'Merge QA',$3,'test','paid','1234',$2) RETURNING id")
        .bind(&suffix).bind(owner).bind(&phone).fetch_one(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO neurovector.payments(order_id,status,amount) VALUES($1,'captured',1000)",
    )
    .bind(order)
    .execute(&pool)
    .await
    .unwrap();

    // When: legacy and order reviews travel through their original public URLs.
    let legacy_input = json!({"item_id":item,"rating":4,"content":"legacy review"});
    payload(
        client
            .post(format!("{base}/reviews"))
            .header("Cookie", &customer_cookie)
            .json(&legacy_input)
            .send()
            .await
            .unwrap(),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    let legacy = payload(
        client
            .post(format!("{base}/reviews"))
            .header("Cookie", &user_cookie)
            .header("Origin", "http://legacy-client.test")
            .json(&legacy_input)
            .send()
            .await
            .unwrap(),
        StatusCode::CREATED,
    )
    .await;
    let input = json!({"rating":5,"content":"order review","tags":["맛있어요"],"photo_data_url":"data:image/png;base64,iVBORw0KGgpmaXh0dXJl"});
    payload(
        client
            .post(format!("{base}/orders/{order}/review"))
            .header("Cookie", &user_cookie)
            .json(&input)
            .send()
            .await
            .unwrap(),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    payload(
        client
            .post(format!("{base}/orders/{order}/review"))
            .header("Cookie", &customer_cookie)
            .header("Origin", "http://foreign-client.test")
            .json(&input)
            .send()
            .await
            .unwrap(),
        StatusCode::BAD_REQUEST,
    )
    .await;
    let review = payload(
        client
            .post(format!("{base}/orders/{order}/review"))
            .header("Cookie", &customer_cookie)
            .json(&input)
            .send()
            .await
            .unwrap(),
        StatusCode::CREATED,
    )
    .await;

    payload(
        client
            .post(format!("{base}/orders/{order}/review"))
            .header("Cookie", &customer_cookie)
            .json(&input)
            .send()
            .await
            .unwrap(),
        StatusCode::CONFLICT,
    )
    .await;
    // Then: item_id selects the legacy array, while order lists retain their envelope.
    let items = payload(
        client
            .get(format!("{base}/reviews?item_id={item}&limit=1&offset=0"))
            .send()
            .await
            .unwrap(),
        StatusCode::OK,
    )
    .await;
    assert_eq!(items[0]["id"], legacy["id"]);
    let orders = payload(
        client
            .get(format!("{base}/reviews?phone={phone}"))
            .send()
            .await
            .unwrap(),
        StatusCode::OK,
    )
    .await;
    assert_eq!(orders["items"][0]["id"], review["id"]);
    assert_eq!(orders["total"], 1);
    assert_eq!(orders["average_rating"], 5.0);
    let all = payload(
        client.get(format!("{base}/reviews")).send().await.unwrap(),
        StatusCode::OK,
    )
    .await;
    assert!(
        all["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == review["id"])
    );
    assert_eq!(
        client
            .get(format!("{base}/reviews?item_id=invalid"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    payload(
        client
            .get(format!("{base}/orders/{order}/review"))
            .header("Cookie", &user_cookie)
            .send()
            .await
            .unwrap(),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    let photo = client
        .get(format!(
            "{base}/reviews/{}/photo",
            review["id"].as_str().unwrap()
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(photo.status(), StatusCode::OK);
    assert_eq!(photo.headers()["content-type"], "image/png");
    assert_eq!(
        photo.bytes().await.unwrap().as_ref(),
        b"\x89PNG\r\n\x1a\nfixture"
    );
    payload(
        client
            .get(format!("{base}/auth/cards"))
            .header("Cookie", &customer_cookie)
            .send()
            .await
            .unwrap(),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        client
            .get(format!("{base}/inventory/photos"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        client
            .get(format!("{base}/auth/oauth/providers"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    payload(
        client
            .post(format!("{base}/orders/{order}/receipt"))
            .header("Cookie", &customer_cookie)
            .json(&json!({"confirmed":true}))
            .send()
            .await
            .unwrap(),
        StatusCode::NOT_FOUND,
    )
    .await;
    server.abort();
    pool.close().await;
}
