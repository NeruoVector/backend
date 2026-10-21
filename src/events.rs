use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use rskafka::{
    client::{
        ClientBuilder,
        partition::{Compression, PartitionClient, UnknownTopicHandling},
    },
    record::Record,
};
use serde::Serialize;
use tokio::sync::OnceCell;

/// Kafka `neurovector.events` 토픽으로 발행하는 이벤트
#[derive(Debug, Serialize)]
pub struct DetectionEvent<'a> {
    pub id: uuid::Uuid,
    pub drone_id: &'a str,
    pub label: &'a str,
    pub confidence: f32,
    pub distance_m: Option<f64>,
    pub priority_level: i16,
    pub detected_at: DateTime<Utc>,
}

/// Kafka producer 를 재사용하는 얇은 래퍼.
///
/// 기존 구현은 이벤트마다 Client 와 PartitionClient 를 새로 만들었는데,
/// MQTT 로 초당 여러 건이 들어오는 환경에서는 커넥션이 폭증한다.
#[derive(Clone)]
pub struct KafkaPublisher {
    brokers: String,
    topic: String,
    partition: Arc<OnceCell<Arc<PartitionClient>>>,
}

impl KafkaPublisher {
    pub fn new(brokers: impl Into<String>, topic: impl Into<String>) -> Self {
        Self {
            brokers: brokers.into(),
            topic: topic.into(),
            partition: Arc::new(OnceCell::new()),
        }
    }

    async fn partition(&self) -> anyhow::Result<Arc<PartitionClient>> {
        let client = self
            .partition
            .get_or_try_init(|| async {
                let brokers = self
                    .brokers
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>();

                let client = ClientBuilder::new(brokers).build().await?;
                let partition = client
                    .partition_client(self.topic.clone(), 0, UnknownTopicHandling::Retry)
                    .await?;

                Ok::<_, anyhow::Error>(Arc::new(partition))
            })
            .await?;

        Ok(Arc::clone(client))
    }

    pub async fn publish_detection(&self, event: &DetectionEvent<'_>) -> anyhow::Result<()> {
        self.publish_json(Some(event.drone_id.as_bytes()), event)
            .await
    }

    /// 임의의 JSON 레코드를 이 publisher 의 토픽으로 발행한다.
    /// key 는 같은 업무 단위의 이벤트를 같은 파티션으로 보내는 데 사용한다.
    pub async fn publish_json<T: Serialize + ?Sized>(
        &self,
        key: Option<&[u8]>,
        event: &T,
    ) -> anyhow::Result<()> {
        let partition = self.partition().await?;
        let payload = serde_json::to_vec(event)?;

        let record = Record {
            key: key.map(|key| key.to_vec()),
            value: Some(payload),
            headers: BTreeMap::new(),
            timestamp: Utc::now(),
        };

        partition
            .produce(vec![record], Compression::NoCompression)
            .await?;
        Ok(())
    }
}
