-- 배송 경로 / 배송 일정 / 비행 세션 / 텔레메트리 / 주문·재고·결제
-- 모든 문장은 반복 실행해도 안전하도록 IF NOT EXISTS 를 사용합니다.

-- ---------------------------------------------------------------- 경로
CREATE TABLE IF NOT EXISTS neurovector.routes (
    id                    uuid PRIMARY KEY DEFAULT uuid_generate_v4(),
    name                  text        NOT NULL,
    drone_id              text,
    waypoints             jsonb       NOT NULL,
    cruise_speed_mps      double precision NOT NULL DEFAULT 8.0,
    max_altitude_m        double precision NOT NULL DEFAULT 120.0,
    distance_m            double precision NOT NULL DEFAULT 0,
    estimated_duration_s  double precision NOT NULL DEFAULT 0,
    created_at            timestamptz NOT NULL DEFAULT now(),
    updated_at            timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS routes_drone_id_idx ON neurovector.routes (drone_id);
CREATE INDEX IF NOT EXISTS routes_created_at_idx ON neurovector.routes (created_at DESC);

-- 경로 검증에 사용하는 비행 금지 구역. polygon 은 [{ "lat": .., "lon": .. }, ...] 형식.
CREATE TABLE IF NOT EXISTS neurovector.no_fly_zones (
    id              uuid PRIMARY KEY DEFAULT uuid_generate_v4(),
    name            text        NOT NULL,
    polygon         jsonb       NOT NULL,
    max_altitude_m  double precision,
    active          boolean     NOT NULL DEFAULT true,
    created_at      timestamptz NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------- 배송 일정
CREATE TABLE IF NOT EXISTS neurovector.deliveries (
    id            uuid PRIMARY KEY DEFAULT uuid_generate_v4(),
    route_id      uuid        NOT NULL REFERENCES neurovector.routes(id) ON DELETE RESTRICT,
    drone_id      text        NOT NULL,
    cargo         text        NOT NULL,
    status        text        NOT NULL DEFAULT 'scheduled'
                  CHECK (status IN ('scheduled', 'dispatched', 'in_flight', 'completed', 'failed', 'cancelled')),
    scheduled_at  timestamptz NOT NULL,
    started_at    timestamptz,
    completed_at  timestamptz,
    notes         text,
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS deliveries_drone_schedule_idx
    ON neurovector.deliveries (drone_id, scheduled_at);
CREATE INDEX IF NOT EXISTS deliveries_status_idx
    ON neurovector.deliveries (status);

-- ---------------------------------------------------------------- 비행 세션 / 텔레메트리
CREATE TABLE IF NOT EXISTS neurovector.flight_sessions (
    id           uuid PRIMARY KEY DEFAULT uuid_generate_v4(),
    drone_id     text        NOT NULL,
    delivery_id  uuid        REFERENCES neurovector.deliveries(id) ON DELETE SET NULL,
    status       text        NOT NULL DEFAULT 'in_flight'
                 CHECK (status IN ('in_flight', 'completed', 'failed', 'aborted')),
    started_at   timestamptz NOT NULL DEFAULT now(),
    ended_at     timestamptz,
    created_at   timestamptz NOT NULL DEFAULT now()
);

-- 드론별 활성 비행은 하나만 허용
CREATE UNIQUE INDEX IF NOT EXISTS flight_sessions_one_active_idx
    ON neurovector.flight_sessions (drone_id)
    WHERE ended_at IS NULL;

CREATE INDEX IF NOT EXISTS flight_sessions_drone_started_idx
    ON neurovector.flight_sessions (drone_id, started_at DESC);

CREATE TABLE IF NOT EXISTS neurovector.telemetry (
    id                  bigserial PRIMARY KEY,
    drone_id            text        NOT NULL,
    flight_id           uuid        REFERENCES neurovector.flight_sessions(id) ON DELETE SET NULL,
    ts                  timestamptz NOT NULL DEFAULT now(),
    latitude            double precision,
    longitude           double precision,
    altitude_m          double precision,
    speed_mps           double precision,
    vertical_speed_mps  double precision,
    battery_pct         double precision,
    heading_deg         double precision,
    status              text,
    raw                 jsonb       NOT NULL DEFAULT '{}'::jsonb,
    received_at         timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS telemetry_drone_ts_idx
    ON neurovector.telemetry (drone_id, ts DESC);
CREATE INDEX IF NOT EXISTS telemetry_flight_idx
    ON neurovector.telemetry (flight_id, ts);

-- ---------------------------------------------------------------- 재고 / 주문 / 결제
CREATE TABLE IF NOT EXISTS neurovector.inventory_items (
    id              uuid PRIMARY KEY DEFAULT uuid_generate_v4(),
    sku             text        NOT NULL UNIQUE,
    name            text        NOT NULL,
    description     text,
    unit_price      bigint      NOT NULL CHECK (unit_price >= 0),
    currency        text        NOT NULL DEFAULT 'KRW',
    stock_quantity  integer     NOT NULL DEFAULT 0 CHECK (stock_quantity >= 0),
    active          boolean     NOT NULL DEFAULT true,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS inventory_items_active_idx
    ON neurovector.inventory_items (active, name);

CREATE TABLE IF NOT EXISTS neurovector.orders (
    id                uuid PRIMARY KEY DEFAULT uuid_generate_v4(),
    order_number      text        NOT NULL UNIQUE,
    customer_name     text        NOT NULL,
    customer_phone    text        NOT NULL,
    status            text        NOT NULL DEFAULT 'pending_payment'
                      CHECK (status IN ('pending_payment', 'paid', 'preparing', 'ready', 'assigned', 'in_transit', 'delivered', 'cancelled', 'failed')),
    total_amount      bigint      NOT NULL DEFAULT 0 CHECK (total_amount >= 0),
    currency          text        NOT NULL DEFAULT 'KRW',
    delivery_address  text        NOT NULL,
    delivery_detail   text,
    latitude          double precision,
    longitude         double precision,
    notes             text,
    delivery_id       uuid        REFERENCES neurovector.deliveries(id) ON DELETE SET NULL,
    stock_restored_at timestamptz,
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS orders_status_created_idx
    ON neurovector.orders (status, created_at DESC);
CREATE INDEX IF NOT EXISTS orders_phone_idx
    ON neurovector.orders (customer_phone);

CREATE TABLE IF NOT EXISTS neurovector.order_items (
    id              uuid PRIMARY KEY DEFAULT uuid_generate_v4(),
    order_id        uuid        NOT NULL REFERENCES neurovector.orders(id) ON DELETE CASCADE,
    item_id         uuid        NOT NULL REFERENCES neurovector.inventory_items(id) ON DELETE RESTRICT,
    quantity        integer     NOT NULL CHECK (quantity > 0),
    unit_price      bigint      NOT NULL CHECK (unit_price >= 0),
    line_total      bigint      NOT NULL CHECK (line_total >= 0),
    created_at      timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS order_items_order_idx ON neurovector.order_items (order_id);

CREATE TABLE IF NOT EXISTS neurovector.payments (
    id              uuid PRIMARY KEY DEFAULT uuid_generate_v4(),
    order_id        uuid        NOT NULL REFERENCES neurovector.orders(id) ON DELETE CASCADE,
    provider        text        NOT NULL DEFAULT 'dummy',
    method          text        NOT NULL DEFAULT 'dummy_card',
    status          text        NOT NULL DEFAULT 'pending'
                    CHECK (status IN ('pending', 'authorized', 'captured', 'failed', 'refunded')),
    amount          bigint      NOT NULL CHECK (amount >= 0),
    currency        text        NOT NULL DEFAULT 'KRW',
    transaction_id  text,
    failure_reason  text,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS payments_order_idx ON neurovector.payments (order_id, created_at DESC);
