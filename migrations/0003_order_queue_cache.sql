-- Kafka 주문 큐 / 주문 관리 이벤트 / 컨슈머 오프셋
-- 반복 실행해도 안전하도록 IF NOT EXISTS 를 사용합니다.

-- 비동기 주문 생성 작업. API 가 job 을 만들고 Kafka 로 job_id 를 발행하면
-- order-worker 가 이 테이블을 원자적으로 선점(claim)해 주문을 생성합니다.
CREATE TABLE IF NOT EXISTS neurovector.order_jobs (
    id             uuid PRIMARY KEY DEFAULT uuid_generate_v4(),
    status         text        NOT NULL DEFAULT 'queued'
                   CHECK (status IN ('queued', 'processing', 'completed', 'failed', 'dead', 'cancelled')),
    payload        jsonb       NOT NULL,
    attempts       integer     NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    max_attempts   integer     NOT NULL DEFAULT 3 CHECK (max_attempts > 0),
    order_id       uuid        REFERENCES neurovector.orders(id) ON DELETE SET NULL,
    last_error     text,
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now(),
    started_at     timestamptz,
    completed_at   timestamptz
);

CREATE INDEX IF NOT EXISTS order_jobs_status_created_idx
    ON neurovector.order_jobs (status, created_at ASC);
CREATE INDEX IF NOT EXISTS order_jobs_order_idx
    ON neurovector.order_jobs (order_id);

-- Kafka 로 발행된 주문/결제 이벤트를 컨슈머가 멱등하게 적재하는 감사 로그.
CREATE TABLE IF NOT EXISTS neurovector.order_events (
    event_id    uuid PRIMARY KEY,
    event_type  text        NOT NULL,
    order_id    uuid,
    job_id      uuid,
    payload     jsonb       NOT NULL DEFAULT '{}'::jsonb,
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS order_events_order_created_idx
    ON neurovector.order_events (order_id, created_at DESC);
CREATE INDEX IF NOT EXISTS order_events_type_created_idx
    ON neurovector.order_events (event_type, created_at DESC);

-- rskafka 는 consumer group offset 관리를 제공하지 않으므로 마지막 처리 오프셋을
-- PostgreSQL 에 저장한다. 처리 후 오프셋을 갱신하며, 재시작 시 이 값 다음부터 읽는다.
CREATE TABLE IF NOT EXISTS neurovector.kafka_consumer_offsets (
    consumer      text        NOT NULL,
    topic         text        NOT NULL,
    partition     integer     NOT NULL,
    next_offset   bigint      NOT NULL DEFAULT 0,
    updated_at    timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (consumer, topic, partition)
);
