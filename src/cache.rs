use axum::http::{HeaderMap, HeaderValue};
use redis::{AsyncCommands, Client};
use serde::{Serialize, de::DeserializeOwned};
use utoipa::ToSchema;

/// Redis cache-aside 래퍼.
///
/// Redis 장애가 API 장애로 이어지지 않도록 모든 캐시 작업은 best-effort 로 동작한다.
#[derive(Clone)]
pub struct Cache {
    client: Client,
    enabled: bool,
    ttl_seconds: u64,
    namespace: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CacheInfo {
    pub enabled: bool,
    pub ttl_seconds: u64,
    pub namespace: String,
    pub keys: u64,
}

impl Cache {
    pub fn new(client: Client, enabled: bool, ttl_seconds: u64) -> Self {
        Self {
            client,
            enabled,
            ttl_seconds: ttl_seconds.max(1),
            namespace: "neurovector:cache:".to_string(),
        }
    }

    pub async fn get<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        if !self.enabled {
            return None;
        }

        let mut conn = self.client.get_multiplexed_async_connection().await.ok()?;
        let raw: Option<String> = conn.get(self.key(key)).await.ok()?;
        match raw {
            Some(raw) => serde_json::from_str(&raw).ok(),
            None => None,
        }
    }

    pub async fn set<T: Serialize + ?Sized>(&self, key: &str, value: &T) -> anyhow::Result<()> {
        self.set_with_ttl(key, value, None).await
    }

    pub async fn set_with_ttl<T: Serialize + ?Sized>(
        &self,
        key: &str,
        value: &T,
        ttl_seconds: Option<u64>,
    ) -> anyhow::Result<()> {
        if !self.enabled {
            return Ok(());
        }

        let payload = serde_json::to_string(value)?;
        let ttl = ttl_seconds.unwrap_or(self.ttl_seconds).max(1);
        let mut conn = self.client.get_multiplexed_async_connection().await?;
        let _: () = conn.set_ex(self.key(key), payload, ttl).await?;
        Ok(())
    }

    /// prefix 로 시작하는 캐시 키를 SCAN 으로 지운다. `KEYS *` 를 사용하지 않는다.
    pub async fn delete_prefix(&self, prefix: &str) -> anyhow::Result<u64> {
        if !self.enabled {
            return Ok(0);
        }

        let pattern = format!("{}*", self.key(prefix));
        let mut conn = self.client.get_multiplexed_async_connection().await?;
        let mut cursor = 0u64;
        let mut deleted = 0u64;

        loop {
            let (next_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(200)
                .query_async(&mut conn)
                .await?;

            if !keys.is_empty() {
                let count: u64 = conn.del(keys).await?;
                deleted += count;
            }

            cursor = next_cursor;
            if cursor == 0 {
                break;
            }
        }

        Ok(deleted)
    }

    pub async fn info(&self) -> CacheInfo {
        let keys = if self.enabled {
            self.count_prefix("").await.unwrap_or(0)
        } else {
            0
        };

        CacheInfo {
            enabled: self.enabled,
            ttl_seconds: self.ttl_seconds,
            namespace: self.namespace.clone(),
            keys,
        }
    }

    async fn count_prefix(&self, prefix: &str) -> anyhow::Result<u64> {
        let pattern = format!("{}*", self.key(prefix));
        let mut conn = self.client.get_multiplexed_async_connection().await?;
        let mut cursor = 0u64;
        let mut count = 0u64;

        loop {
            let (next_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(500)
                .query_async(&mut conn)
                .await?;
            count += keys.len() as u64;

            cursor = next_cursor;
            if cursor == 0 {
                break;
            }
        }

        Ok(count)
    }

    /// Redis TTL 연장 없이 키 존재 여부만 확인한다. 캐시 히트 헤더용.
    pub fn headers(hit: bool) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-cache",
            HeaderValue::from_static(if hit { "HIT" } else { "MISS" }),
        );
        headers
    }

    fn key(&self, key: &str) -> String {
        let key = key.trim_start_matches(':');
        format!("{}{}", self.namespace, key)
    }
}
