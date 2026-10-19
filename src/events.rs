use std::collections::BTreeMap;

use chrono::Utc;
use rskafka::{
    client::{
        ClientBuilder,
        partition::{Compression, PartitionClient, UnknownTopicHandling},
    },
    record::Record,
};
use serde::Serialize;

/// Kafka `neurovector.events` 토픽으로 발행하는 이벤트
#[derive(Debug, Serialize)]
pub struct DetectionEvent<'a> {
    pub id: uuid::Uuid,
    pub drone_id: &'a str,
    pub label: &'a str,
    pub confidence: f32,
    pub detected_at: chrono::DateTime<Utc>,
}

/// 이벤트를 Kafka 로 발행한다.
///
/// 실패해도 호출한 쪽의 주 흐름(DB 저장 등)을 막지 않도록
/// 반환값을 호출부에서 로그로만 처리한다.
pub async fn publish_detection(
    brokers: &str,
    topic: &str,
    event: &DetectionEvent<'_>,
) -> anyhow::Result<()> {
    let client = ClientBuilder::new(brokers.split(',').map(str::to_string).collect())
        .build()
        .await?;

    let partition = client
        .partition_client(topic, 0, UnknownTopicHandling::Retry)
        .await?;

    produce(&partition, event).await
}

async fn produce(partition: &PartitionClient, event: &DetectionEvent<'_>) -> anyhow::Result<()> {
    let payload = serde_json::to_vec(event)?;

    let record = Record {
        key: Some(event.drone_id.as_bytes().to_vec()),
        value: Some(payload),
        headers: BTreeMap::new(),
        timestamp: Utc::now(),
    };

    partition
        .produce(vec![record], Compression::NoCompression)
        .await?;
    Ok(())
}
