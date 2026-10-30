use std::sync::Arc;
use std::time::Duration;

use crate::db::PgPool;
use chrono::{DateTime, Utc};
use futures::StreamExt;
use rskafka::client::{
    ClientBuilder,
    consumer::{StartOffset, StreamConsumerBuilder},
    partition::UnknownTopicHandling,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    config::Config,
    error::AppError,
    routes::orders::{self, CreateOrder},
    state::AppState,
};

const WORKER_NAME: &str = "order-worker";
const STALE_JOB_MINUTES: i64 = 10;

/// Kafka 주문 토픽에 실리는 메시지.
///
/// `CreateOrder` 는 비동기 주문 생성 작업이고, `OrderEvent` 는 주문/결제 상태 변화를
/// 관리용 감사 로그로 적재하기 위한 이벤트입니다. 모두 `event_id` 를 가지고 있어
/// 컨슈머가 재처리하더라도 멱등하게 동작합니다.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OrderQueueMessage {
    CreateOrder {
        event_id: Uuid,
        job_id: Uuid,
    },
    OrderEvent {
        event_id: Uuid,
        event_type: String,
        order_id: Uuid,
        job_id: Option<Uuid>,
        payload: Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct OrderJob {
    pub id: Uuid,
    pub status: String,
    #[schema(value_type = Object)]
    pub payload: Value,
    pub attempts: i32,
    pub max_attempts: i32,
    pub order_id: Option<Uuid>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}
crate::db::row!(OrderJob {
    id,
    status,
    payload,
    attempts,
    max_attempts,
    order_id,
    last_error,
    created_at,
    updated_at,
    started_at,
    completed_at
});

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct OrderEvent {
    pub event_id: Uuid,
    pub event_type: String,
    pub order_id: Option<Uuid>,
    pub job_id: Option<Uuid>,
    #[schema(value_type = Object)]
    pub payload: Value,
    pub created_at: DateTime<Utc>,
}
crate::db::row!(OrderEvent {
    event_id,
    event_type,
    order_id,
    job_id,
    payload,
    created_at
});

#[derive(Debug, Serialize, ToSchema)]
pub struct EnqueueOrderResponse {
    pub job: OrderJob,
    /// Kafka 발행까지 성공했는지 여부. false 이면 job 은 queued 상태로 남고
    /// 관리자가 retry 할 수 있습니다.
    pub kafka_published: bool,
}

/// 주문 생성 요청을 order_jobs 에 저장하고 Kafka 로 job_id 를 발행한다.
pub async fn enqueue_create_order(
    state: &AppState,
    body: CreateOrder,
) -> Result<EnqueueOrderResponse, AppError> {
    orders::validate_create_order(&body)?;
    if body.handoff_pin.is_some() || body.spot_id.is_some() {
        return Err(AppError::BadRequest(
            "customer handoff and saved spot checkout requires the synchronous orders endpoint"
                .into(),
        ));
    }
    let payload = serde_json::to_value(&body).map_err(anyhow::Error::from)?;

    let job = crate::db::fetch_one::<OrderJob>(
        &state.pool.get().await?,
        r#"
        INSERT INTO neurovector.order_jobs (payload)
        VALUES ($1)
        RETURNING id, status, payload, attempts, max_attempts, order_id, last_error,
                  created_at, updated_at, started_at, completed_at
        "#,
        &[&(payload)],
    )
    .await?;

    let kafka_published = publish_create_job(state, job.id).await;
    Ok(EnqueueOrderResponse {
        job,
        kafka_published,
    })
}

async fn publish_create_job(state: &AppState, job_id: Uuid) -> bool {
    let message = OrderQueueMessage::CreateOrder {
        event_id: Uuid::new_v4(),
        job_id,
    };

    match tokio::time::timeout(
        Duration::from_secs(1),
        state
            .order_queue
            .publish_json(Some(job_id.as_bytes()), &message),
    )
    .await
    {
        Ok(Ok(())) => {
            if let Err(err) = async { crate::db::execute(&state.pool.get().await?, "UPDATE neurovector.order_jobs SET updated_at = now() WHERE id = $1 AND status = 'queued'", &[&(job_id)])
            .await?; Ok::<(), AppError>(()) }.await
            {
                tracing::warn!(error = %err, %job_id, "failed to mark order job as published");
            }
            true
        }
        Ok(Err(err)) => {
            tracing::warn!(error = %err, %job_id, "failed to publish order job");
            false
        }
        Err(_) => {
            tracing::warn!(%job_id, "timed out publishing order job");
            false
        }
    }
}

/// 주문/결제 상태 이벤트를 order_events 에 적재하고 Kafka 로도 발행한다.
///
/// Kafka 장애가 주문 API 를 실패시키지 않도록 best-effort 로 처리하되, 로컬 감사
/// 로그는 먼저 남겨서 관리 화면에서 이벤트를 확인할 수 있게 한다. 컨슈머는 같은
/// event_id 를 ON CONFLICT 로 무시하므로 재처리에도 안전하다.
pub async fn publish_order_event(
    state: &AppState,
    event_type: &str,
    order_id: Uuid,
    job_id: Option<Uuid>,
    payload: Value,
) {
    let event_id = Uuid::new_v4();

    if let Err(err) = async {
        crate::db::execute(
            &state.pool.get().await?,
            r#"
        INSERT INTO neurovector.order_events
            (event_id, event_type, order_id, job_id, payload)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (event_id) DO NOTHING
        "#,
            &[
                &(event_id),
                &(event_type),
                &(order_id),
                &(job_id),
                &(&payload),
            ],
        )
        .await?;
        Ok::<(), AppError>(())
    }
    .await
    {
        tracing::warn!(error = %err, %order_id, event_type, "failed to persist order event");
    }

    let message = OrderQueueMessage::OrderEvent {
        event_id,
        event_type: event_type.to_string(),
        order_id,
        job_id,
        payload,
    };
    let event_type = event_type.to_string();
    let publisher = state.order_queue.clone();

    // Kafka 발행은 응답/컨슈머 처리 시간에 영향을 주지 않도록 백그라운드로 넘긴다.
    // 감사 로그는 위에서 이미 PostgreSQL 에 기록했으므로 발행 실패가 주문을 실패시키지 않는다.
    tokio::spawn(async move {
        match tokio::time::timeout(
            Duration::from_secs(1),
            publisher.publish_json(Some(order_id.as_bytes()), &message),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                tracing::warn!(error = %err, %order_id, event_type, "failed to publish order event");
            }
            Err(_) => {
                tracing::warn!(%order_id, event_type, "timed out publishing order event");
            }
        }
    });
}

pub async fn get_job(pool: &PgPool, id: Uuid) -> Result<OrderJob, AppError> {
    crate::db::fetch_optional::<OrderJob>(
        &pool.get().await?,
        r#"
        SELECT id, status, payload, attempts, max_attempts, order_id, last_error,
               created_at, updated_at, started_at, completed_at
        FROM neurovector.order_jobs
        WHERE id = $1
        "#,
        &[&(id)],
    )
    .await?
    .ok_or(AppError::NotFound)
}

/// 실패/대기 상태의 job 을 다시 queued 로 만들고 Kafka 로 재발행한다.
pub async fn retry_job(state: &AppState, id: Uuid) -> Result<EnqueueOrderResponse, AppError> {
    let job = crate::db::fetch_optional::<OrderJob>(
        &state.pool.get().await?,
        r#"
        UPDATE neurovector.order_jobs
        SET status = 'queued',
            last_error = NULL,
            started_at = NULL,
            completed_at = NULL,
            updated_at = now()
        WHERE id = $1 AND status IN ('queued', 'failed', 'dead', 'cancelled')
        RETURNING id, status, payload, attempts, max_attempts, order_id, last_error,
                  created_at, updated_at, started_at, completed_at
        "#,
        &[&(id)],
    )
    .await?
    .ok_or_else(|| {
        AppError::Conflict("only queued, failed, dead or cancelled jobs can be retried".into())
    })?;

    let kafka_published = publish_create_job(state, job.id).await;
    Ok(EnqueueOrderResponse {
        job,
        kafka_published,
    })
}

pub async fn cancel_job(state: &AppState, id: Uuid) -> Result<OrderJob, AppError> {
    crate::db::fetch_optional::<OrderJob>(
        &state.pool.get().await?,
        r#"
        UPDATE neurovector.order_jobs
        SET status = 'cancelled',
            completed_at = now(),
            updated_at = now()
        WHERE id = $1 AND status IN ('queued', 'failed', 'dead')
        RETURNING id, status, payload, attempts, max_attempts, order_id, last_error,
                  created_at, updated_at, started_at, completed_at
        "#,
        &[&(id)],
    )
    .await?
    .ok_or_else(|| AppError::Conflict("only queued, failed or dead jobs can be cancelled".into()))
}

/// Kafka 컨슈머를 백그라운드에서 실행한다. 브로커가 내려가 있어도 서비스 시작을
/// 막지 않고 재연결을 반복한다.
pub fn spawn_worker(state: AppState, config: &Config) {
    let brokers = config.kafka_brokers.clone();
    let topic = config.kafka_topic_orders.clone();

    let republisher_state = state.clone();
    tokio::spawn(async move {
        if let Err(err) = recover_stale_jobs(&republisher_state.pool).await {
            tracing::warn!(error = %err, "failed to recover stale order jobs");
        }

        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            if let Err(err) = republish_queued_jobs(&republisher_state).await {
                tracing::warn!(error = %err, "failed to republish queued order jobs");
            }
        }
    });

    tokio::spawn(async move {
        loop {
            if let Err(err) = run_consumer(&state, &brokers, &topic).await {
                tracing::warn!(error = %err, "order queue consumer stopped; reconnecting");
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
}

/// Kafka 발행에 실패했거나 브로커가 늦게 뜬 경우를 대비해 오래된 queued job 을
/// 주기적으로 다시 발행한다. 중복 메시지는 worker 의 원자적 claim 으로 무시된다.
async fn republish_queued_jobs(state: &AppState) -> anyhow::Result<()> {
    let job_ids = crate::db::scalar_all::<Uuid>(
        &state.pool.get().await?,
        r#"
        SELECT id
        FROM neurovector.order_jobs
        WHERE status = 'queued'
          AND updated_at < now() - interval '30 seconds'
        ORDER BY created_at ASC
        LIMIT 50
        "#,
        &[],
    )
    .await?;

    for job_id in job_ids {
        if publish_create_job(state, job_id).await {
            tracing::info!(%job_id, "republished queued order job");
        }
    }
    Ok(())
}

async fn run_consumer(state: &AppState, brokers: &str, topic: &str) -> anyhow::Result<()> {
    let broker_list = brokers
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();

    let client = ClientBuilder::new(broker_list).build().await?;
    let partition_client = Arc::new(
        client
            .partition_client(topic.to_string(), 0, UnknownTopicHandling::Retry)
            .await?,
    );

    let next_offset = load_offset(&state.pool, topic).await?;
    let start = if next_offset <= 0 {
        StartOffset::Earliest
    } else {
        StartOffset::At(next_offset)
    };

    let mut stream = StreamConsumerBuilder::new(partition_client, start)
        .with_max_wait_ms(500)
        .build();

    tracing::info!(topic, next_offset, "order queue consumer started");

    while let Some(result) = stream.next().await {
        let (record, _) = match result {
            Ok(record) => record,
            Err(err) => {
                // 토픽이 재생성되거나 보존 기간이 지나 저장된 오프셋이 사라진 경우
                // 처음부터 다시 읽는다. job/event 처리는 모두 멱등하다.
                if format!("{err:?}").contains("OffsetOutOfRange") {
                    tracing::warn!(
                        topic,
                        "saved consumer offset is out of range; restarting from earliest"
                    );
                    save_offset(&state.pool, topic, 0).await?;
                    return Ok(());
                }
                return Err(err.into());
            }
        };
        let next = record.offset + 1;

        if let Some(payload) = record.record.value.as_deref() {
            match serde_json::from_slice::<OrderQueueMessage>(payload) {
                Ok(message) => {
                    if let Err(err) = handle_message(state, message).await {
                        // DB 오류 등으로 처리하지 못했다면 오프셋을 전진시키지 않고
                        // 재연결 후 같은 레코드를 다시 읽는다. job 처리는 멱등하다.
                        tracing::error!(
                            error = %err,
                            offset = record.offset,
                            "failed to handle order queue message"
                        );
                        return Err(err);
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        offset = record.offset,
                        "skipping malformed order queue message"
                    );
                }
            }
        }

        save_offset(&state.pool, topic, next).await?;
    }

    Ok(())
}

async fn handle_message(state: &AppState, message: OrderQueueMessage) -> anyhow::Result<()> {
    match message {
        OrderQueueMessage::CreateOrder { job_id, .. } => {
            process_create_order_job(state, job_id).await?;
        }
        OrderQueueMessage::OrderEvent {
            event_id,
            event_type,
            order_id,
            job_id,
            payload,
        } => {
            crate::db::execute(
                &state.pool.get().await?,
                r#"
                INSERT INTO neurovector.order_events
                    (event_id, event_type, order_id, job_id, payload)
                VALUES ($1, $2, $3, $4, $5)
                ON CONFLICT (event_id) DO NOTHING
                "#,
                &[
                    &(event_id),
                    &(&event_type),
                    &(order_id),
                    &(job_id),
                    &(&payload),
                ],
            )
            .await?;
        }
    }

    Ok(())
}

async fn process_create_order_job(state: &AppState, job_id: Uuid) -> anyhow::Result<()> {
    let Some(job) = claim_job(&state.pool, job_id).await? else {
        return Ok(());
    };

    let body: CreateOrder = match serde_json::from_value(job.payload.clone()) {
        Ok(body) => body,
        Err(err) => {
            fail_job(&state.pool, job_id, format!("invalid job payload: {err}")).await?;
            return Ok(());
        }
    };

    if let Err(err) = orders::validate_create_order(&body) {
        fail_job(&state.pool, job_id, err.to_string()).await?;
        return Ok(());
    }
    if body.handoff_pin.is_some() || body.spot_id.is_some() {
        fail_job(
            &state.pool,
            job_id,
            "customer handoff and saved spot checkout requires synchronous checkout".into(),
        )
        .await?;
        return Ok(());
    }

    let mut connection = state.pool.get().await?;
    let mut tx = connection.transaction().await?;
    let detail = match orders::create_order_in_tx(&mut tx, &body).await {
        Ok(detail) => detail,
        Err(err) => {
            if let Err(rollback_error) = tx.rollback().await {
                tracing::warn!(%rollback_error, "order job rollback failed");
            }
            drop(connection);
            fail_job(&state.pool, job_id, err.to_string()).await?;
            return Ok(());
        }
    };

    // 주문 생성과 job 완료를 같은 트랜잭션으로 커밋한다. 그래야 커밋 직후
    // 프로세스가 죽어도 주문만 생성되고 job 이 processing 에 남는 일이 없다.
    if let Err(err) = crate::db::execute(
        &tx,
        r#"
        UPDATE neurovector.order_jobs
        SET status = 'completed', order_id = $2, completed_at = now(), updated_at = now()
        WHERE id = $1 AND status = 'processing'
        "#,
        &[&(job_id), &(detail.order.id)],
    )
    .await
    {
        if let Err(rollback_error) = tx.rollback().await {
            tracing::warn!(%rollback_error, "order job rollback failed");
        }
        drop(connection);
        fail_job(&state.pool, job_id, err.to_string()).await?;
        return Ok(());
    }

    let committed = tx.commit().await;
    drop(connection);
    if let Err(err) = committed {
        fail_job(&state.pool, job_id, err.to_string()).await?;
        return Ok(());
    }

    orders::invalidate_order_cache(state).await;

    publish_order_event(
        state,
        "order.created",
        detail.order.id,
        Some(job_id),
        json!({
            "order_number": detail.order.order_number,
            "status": detail.order.status,
            "total_amount": detail.order.total_amount,
            "currency": detail.order.currency,
            "source": "kafka_queue",
        }),
    )
    .await;

    tracing::info!(%job_id, order_id = %detail.order.id, "order job completed");
    Ok(())
}

async fn claim_job(pool: &PgPool, id: Uuid) -> anyhow::Result<Option<OrderJob>> {
    let job = crate::db::fetch_optional::<OrderJob>(
        &pool.get().await?,
        r#"
        UPDATE neurovector.order_jobs
        SET status = 'processing',
            attempts = attempts + 1,
            started_at = COALESCE(started_at, now()),
            updated_at = now(),
            last_error = NULL
        WHERE id = $1 AND status IN ('queued', 'failed')
        RETURNING id, status, payload, attempts, max_attempts, order_id, last_error,
                  created_at, updated_at, started_at, completed_at
        "#,
        &[&(id)],
    )
    .await?;

    Ok(job)
}

async fn fail_job(pool: &PgPool, id: Uuid, error: String) -> anyhow::Result<()> {
    crate::db::execute(
        &pool.get().await?,
        r#"
        UPDATE neurovector.order_jobs
        SET status = CASE WHEN attempts >= max_attempts THEN 'dead' ELSE 'failed' END,
            last_error = $2,
            completed_at = CASE WHEN attempts >= max_attempts THEN now() ELSE NULL END,
            updated_at = now()
        WHERE id = $1
        "#,
        &[&(id), &(error)],
    )
    .await?;
    Ok(())
}

async fn recover_stale_jobs(pool: &PgPool) -> anyhow::Result<()> {
    let result = crate::db::execute(
        &pool.get().await?,
        r#"
        UPDATE neurovector.order_jobs
        SET status = 'failed',
            last_error = 'worker stopped while processing; recovered on startup',
            updated_at = now()
        WHERE status = 'processing'
          AND started_at < now() - make_interval(mins => $1)
        "#,
        &[&(STALE_JOB_MINUTES as i32)],
    )
    .await?;

    if result > 0 {
        tracing::warn!(count = result, "recovered stale order jobs");
    }
    Ok(())
}

async fn load_offset(pool: &PgPool, topic: &str) -> anyhow::Result<i64> {
    let offset = crate::db::scalar_optional::<i64>(
        &pool.get().await?,
        r#"
        SELECT next_offset
        FROM neurovector.kafka_consumer_offsets
        WHERE consumer = $1 AND topic = $2 AND partition = 0
        "#,
        &[&(WORKER_NAME), &(topic)],
    )
    .await?
    .unwrap_or(0);

    Ok(offset)
}

async fn save_offset(pool: &PgPool, topic: &str, next_offset: i64) -> anyhow::Result<()> {
    crate::db::execute(
        &pool.get().await?,
        r#"
        INSERT INTO neurovector.kafka_consumer_offsets
            (consumer, topic, partition, next_offset, updated_at)
        VALUES ($1, $2, 0, $3, now())
        ON CONFLICT (consumer, topic, partition)
        DO UPDATE SET next_offset = EXCLUDED.next_offset, updated_at = now()
        "#,
        &[&(WORKER_NAME), &(topic), &(next_offset)],
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_state;

    #[tokio::test]
    async fn processes_create_order_job_idempotently() -> anyhow::Result<()> {
        let Ok(database_url) = std::env::var("TEST_DATABASE_URL") else {
            eprintln!("TEST_DATABASE_URL not set; skipping PostgreSQL queue integration test");
            return Ok(());
        };

        let pool = crate::db::pool(&database_url, 2).expect("connect test database");
        let state = test_state(pool.clone());

        let sku = format!("QUEUE-TEST-{}", Uuid::new_v4());
        let item_id = crate::db::scalar_one::<Uuid>(
            &pool.get().await?,
            r#"
            INSERT INTO neurovector.inventory_items
                (sku, name, unit_price, currency, stock_quantity)
            VALUES ($1, 'Queue Test Item', 1000, 'KRW', 3)
            RETURNING id
            "#,
            &[&(&sku)],
        )
        .await
        .expect("insert inventory item");

        let body = CreateOrder {
            customer_name: "Queue Test".into(),
            customer_phone: format!("010-{}", Uuid::new_v4().simple()),
            items: vec![orders::CreateOrderItem {
                item_id,
                quantity: 2,
            }],
            delivery_address: "Test address".into(),
            delivery_detail: None,
            latitude: None,
            longitude: None,
            notes: None,
            delivery_id: None,
            spot_id: None,
            room_number: None,
            grade: None,
            classroom: None,
            handoff_pin: None,
        };
        let payload = serde_json::to_value(&body).expect("serialize order payload");
        let job_id = crate::db::scalar_one::<Uuid>(
            &pool.get().await?,
            "INSERT INTO neurovector.order_jobs (payload) VALUES ($1) RETURNING id",
            &[&(payload)],
        )
        .await
        .expect("insert order job");

        let create_message = OrderQueueMessage::CreateOrder {
            event_id: Uuid::new_v4(),
            job_id,
        };
        handle_message(&state, create_message.clone())
            .await
            .expect("process job");
        // 같은 메시지가 다시 들어와도 주문이 중복 생성되지 않아야 한다.
        handle_message(&state, create_message)
            .await
            .expect("reprocess job is a no-op");

        let (status, order_id, attempts) = crate::db::fetch_one::<(String, Option<Uuid>, i32)>(
            &pool.get().await?,
            "SELECT status, order_id, attempts FROM neurovector.order_jobs WHERE id = $1",
            &[&(job_id)],
        )
        .await
        .expect("fetch job");
        assert_eq!(status, "completed");
        assert_eq!(attempts, 1);
        let order_id = order_id.expect("job has order id");

        let stock = crate::db::scalar_one::<i32>(
            &pool.get().await?,
            "SELECT stock_quantity FROM neurovector.inventory_items WHERE id = $1",
            &[&(item_id)],
        )
        .await
        .expect("fetch stock");
        assert_eq!(stock, 1);

        let order_count = crate::db::scalar_one::<i64>(
            &pool.get().await?,
            "SELECT count(*) FROM neurovector.orders WHERE id = $1",
            &[&(order_id)],
        )
        .await
        .expect("count order");
        assert_eq!(order_count, 1);

        let event_count = crate::db::scalar_one::<i64>(&pool.get().await?, "SELECT count(*) FROM neurovector.order_events WHERE order_id = $1 AND event_type = 'order.created'", &[&(order_id)])
        .await
        .expect("count events");
        assert_eq!(event_count, 1);

        let event_id = Uuid::new_v4();
        let event_message = OrderQueueMessage::OrderEvent {
            event_id,
            event_type: "test.event".into(),
            order_id,
            job_id: Some(job_id),
            payload: json!({"source": "test"}),
        };
        handle_message(&state, event_message.clone())
            .await
            .expect("persist event");
        handle_message(&state, event_message)
            .await
            .expect("replayed event is a no-op");
        let event_count = crate::db::scalar_one::<i64>(
            &pool.get().await?,
            "SELECT count(*) FROM neurovector.order_events WHERE event_id = $1",
            &[&(event_id)],
        )
        .await
        .expect("count test event");
        assert_eq!(event_count, 1);

        // 테스트 데이터 정리
        crate::db::execute(
            &pool.get().await?,
            "DELETE FROM neurovector.order_events WHERE order_id = $1",
            &[&(order_id)],
        )
        .await
        .ok();
        crate::db::execute(
            &pool.get().await?,
            "DELETE FROM neurovector.order_jobs WHERE id = $1",
            &[&(job_id)],
        )
        .await
        .ok();
        crate::db::execute(
            &pool.get().await?,
            "DELETE FROM neurovector.orders WHERE id = $1",
            &[&(order_id)],
        )
        .await
        .ok();
        crate::db::execute(
            &pool.get().await?,
            "DELETE FROM neurovector.inventory_items WHERE id = $1",
            &[&(item_id)],
        )
        .await
        .ok();
        Ok(())
    }
}
