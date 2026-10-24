use crate::redis_store::RedisStore;
use axum::http::{HeaderMap, HeaderValue};
use redis::AsyncCommands;
use serde::{Serialize, de::DeserializeOwned};
use utoipa::ToSchema;

#[cfg(test)]
#[path = "cache_tests.rs"]
mod tests;

/// Redis cache-aside 래퍼.
///
/// Redis 장애가 API 장애로 이어지지 않도록 모든 캐시 작업은 best-effort 로 동작한다.
#[derive(Clone)]
pub struct Cache {
    client: RedisStore,
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

pub struct CacheEntry {
    key: String,
    generation: Option<String>,
}

impl Cache {
    pub fn new(client: RedisStore, enabled: bool, ttl_seconds: u64) -> Self {
        Self {
            client,
            enabled,
            ttl_seconds: ttl_seconds.max(1),
            namespace: "neurovector:cache:".to_string(),
        }
    }

    pub async fn entry(&self, key: &str) -> CacheEntry {
        let generation = if self.enabled {
            self.client
                .execute(|mut conn| async move {
                    redis::cmd("EVAL").arg(
                        "redis.call('SET', KEYS[1], ARGV[1], 'NX'); return redis.call('GET', KEYS[1])",
                    )
                    .arg(1).arg("neurovector:cache-generation")
                    .arg(uuid::Uuid::new_v4().to_string())
                    .query_async::<String>(&mut conn)
                    .await
                })
                .await
                .ok()
        } else {
            None
        };
        CacheEntry {
            key: self.key(key),
            generation,
        }
    }

    pub async fn get<T: DeserializeOwned>(&self, entry: &CacheEntry) -> Option<T> {
        if !self.enabled {
            return None;
        }

        let raw: Option<String> = self
            .client
            .execute(|mut conn| async move { conn.get(&entry.key).await })
            .await
            .ok()?;
        match raw {
            Some(raw) => serde_json::from_str(&raw).ok(),
            None => None,
        }
    }

    pub async fn set<T: Serialize + ?Sized>(
        &self,
        entry: &CacheEntry,
        value: &T,
    ) -> anyhow::Result<()> {
        self.set_with_ttl(entry, value, None).await
    }

    pub async fn set_with_ttl<T: Serialize + ?Sized>(
        &self,
        entry: &CacheEntry,
        value: &T,
        ttl_seconds: Option<u64>,
    ) -> anyhow::Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let Some(generation) = &entry.generation else {
            return Ok(());
        };

        let payload = serde_json::to_string(value)?;
        let ttl = ttl_seconds.unwrap_or(self.ttl_seconds).max(1);
        self.client
            .execute(|mut conn| async move {
                redis::cmd("EVAL").arg(
                    "if redis.call('GET', KEYS[1]) == ARGV[1] then return redis.call('SET', KEYS[2], ARGV[2], 'EX', ARGV[3]) end",
                )
                .arg(2).arg("neurovector:cache-generation")
                .arg(&entry.key)
                .arg(generation)
                .arg(payload)
                .arg(ttl)
                .query_async::<Option<String>>(&mut conn)
                .await
            })
            .await?;
        Ok(())
    }

    /// Invalidate related namespaces in one scan; UNLINK frees values off the
    /// Redis command thread. Prefixes are internal application constants.
    pub async fn delete_prefix(&self, prefix: &str) -> anyhow::Result<u64> {
        self.delete_prefixes(&[prefix]).await
    }

    pub async fn delete_prefixes(&self, prefixes: &[&str]) -> anyhow::Result<u64> {
        if !self.enabled || prefixes.is_empty() {
            return Ok(0);
        }
        let prefixes = prefixes
            .iter()
            .map(|prefix| self.key(prefix))
            .collect::<Vec<_>>();
        let pattern = if prefixes.len() == 1 {
            format!("{}*", prefixes[0])
        } else {
            format!("{}*", self.namespace)
        };
        self.client
            .execute(|mut conn| async move {
                // In-flight readers must not refill keys after invalidation.
                conn.set::<_, _, ()>(
                    "neurovector:cache-generation",
                    uuid::Uuid::new_v4().to_string(),
                )
                .await?;
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
                    let keys = keys
                        .into_iter()
                        .filter(|key| prefixes.iter().any(|prefix| key.starts_with(prefix)))
                        .collect::<Vec<_>>();
                    if !keys.is_empty() {
                        let count: u64 = redis::cmd("UNLINK")
                            .arg(keys)
                            .query_async(&mut conn)
                            .await?;
                        deleted += count;
                    }
                    cursor = next_cursor;
                    if cursor == 0 {
                        break;
                    }
                }
                Ok(deleted)
            })
            .await
            .map_err(Into::into)
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
        self.client
            .execute(|mut conn| async move {
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
            })
            .await
            .map_err(Into::into)
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
