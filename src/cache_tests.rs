use super::*;
use std::time::Duration;

#[tokio::test]
async fn rejects_stale_refill_after_invalidation() {
    let Ok(url) = std::env::var("TEST_REDIS_URL") else {
        eprintln!("TEST_REDIS_URL not set; skipping Redis integration test");
        return;
    };
    let cache = Cache::new(
        RedisStore::new(
            redis::Client::open(url).expect("redis URL"),
            Duration::from_secs(2),
        ),
        true,
        30,
    );
    let prefix = format!("test:{}:", uuid::Uuid::new_v4());
    let key = format!("{prefix}order");
    let stale = cache.entry(&key).await;
    cache.set(&stale, &"old").await.expect("prime cache");
    assert_eq!(cache.get::<String>(&stale).await.as_deref(), Some("old"));

    // Given a reader that started before a concurrent mutation, when it refills late.
    cache.delete_prefix(&prefix).await.expect("invalidate");
    cache
        .set(&stale, &"old")
        .await
        .expect("stale fill is ignored");

    // Then the deleted value stays absent and a fresh read can cache the new value.
    let fresh = cache.entry(&key).await;
    assert_eq!(cache.get::<String>(&fresh).await, None);
    cache.set(&fresh, &"new").await.expect("fresh fill");
    assert_eq!(cache.get::<String>(&fresh).await.as_deref(), Some("new"));
    cache.delete_prefix(&prefix).await.expect("cleanup");
}
