CREATE TABLE IF NOT EXISTS neurovector.delivery_spots (
    id               uuid PRIMARY KEY DEFAULT uuid_generate_v4(),
    customer_id      uuid NOT NULL REFERENCES neurovector.customers(id) ON DELETE CASCADE,
    label            text NOT NULL CHECK (length(btrim(label)) BETWEEN 1 AND 60),
    delivery_address text NOT NULL CHECK (length(btrim(delivery_address)) BETWEEN 1 AND 300),
    delivery_detail  text,
    latitude         double precision NOT NULL CHECK (latitude BETWEEN -90 AND 90),
    longitude        double precision NOT NULL CHECK (longitude BETWEEN -180 AND 180),
    room_number      text,
    grade            integer,
    classroom        text,
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now(),
    CHECK (
        (room_number IS NULL AND grade IS NULL AND classroom IS NULL)
        OR (room_number IS NOT NULL AND length(btrim(room_number)) BETWEEN 1 AND 40
            AND grade IS NULL AND classroom IS NULL)
        OR
        (room_number IS NULL AND grade IS NOT NULL AND grade BETWEEN 1 AND 12
            AND classroom IS NOT NULL AND length(btrim(classroom)) BETWEEN 1 AND 40)
    )
);

DO $$
DECLARE
    constraint_name text;
BEGIN
    FOR constraint_name IN
        SELECT conname
        FROM pg_constraint
        WHERE conrelid = 'neurovector.delivery_spots'::regclass
          AND contype = 'c'
          AND pg_get_constraintdef(oid) ~* '(room_number|grade|classroom)'
    LOOP
        EXECUTE format(
            'ALTER TABLE neurovector.delivery_spots DROP CONSTRAINT %I',
            constraint_name
        );
    END LOOP;

    ALTER TABLE neurovector.delivery_spots
        ADD CONSTRAINT delivery_spots_indoor_destination_check CHECK (
            (room_number IS NULL AND grade IS NULL AND classroom IS NULL)
            OR (room_number IS NOT NULL AND length(btrim(room_number)) BETWEEN 1 AND 40
                AND grade IS NULL AND classroom IS NULL)
            OR (room_number IS NULL AND grade IS NOT NULL AND grade BETWEEN 1 AND 12
                AND classroom IS NOT NULL AND length(btrim(classroom)) BETWEEN 1 AND 40)
        );
END $$;

CREATE INDEX IF NOT EXISTS delivery_spots_customer_created_idx
    ON neurovector.delivery_spots (customer_id, created_at DESC);

ALTER TABLE neurovector.orders
    ADD COLUMN IF NOT EXISTS customer_id uuid REFERENCES neurovector.customers(id) ON DELETE SET NULL,
    ADD COLUMN IF NOT EXISTS spot_id uuid REFERENCES neurovector.delivery_spots(id) ON DELETE SET NULL,
    ADD COLUMN IF NOT EXISTS room_number text,
    ADD COLUMN IF NOT EXISTS grade integer,
    ADD COLUMN IF NOT EXISTS classroom text;

DO $$
DECLARE
    constraint_name text;
BEGIN
    FOR constraint_name IN
        SELECT conname
        FROM pg_constraint
        WHERE conrelid = 'neurovector.orders'::regclass
          AND contype = 'c'
          AND pg_get_constraintdef(oid) ~* '(room_number|grade|classroom)'
    LOOP
        EXECUTE format(
            'ALTER TABLE neurovector.orders DROP CONSTRAINT %I',
            constraint_name
        );
    END LOOP;

    ALTER TABLE neurovector.orders
        ADD CONSTRAINT orders_delivery_indoor_destination_check CHECK (
            (room_number IS NULL AND grade IS NULL AND classroom IS NULL)
            OR (room_number IS NOT NULL AND length(btrim(room_number)) BETWEEN 1 AND 40
                AND grade IS NULL AND classroom IS NULL)
            OR (room_number IS NULL AND grade IS NOT NULL AND grade BETWEEN 1 AND 12
                AND classroom IS NOT NULL AND length(btrim(classroom)) BETWEEN 1 AND 40)
        );
END $$;

ALTER TABLE neurovector.product_details
    ADD COLUMN IF NOT EXISTS weight_grams integer CHECK (weight_grams IS NULL OR weight_grams > 0);

ALTER TABLE neurovector.order_items
    ADD COLUMN IF NOT EXISTS weight_grams integer CHECK (weight_grams IS NULL OR weight_grams > 0);

CREATE TABLE IF NOT EXISTS neurovector.delivery_fleet (
    drone_id          text PRIMARY KEY CHECK (length(btrim(drone_id)) BETWEEN 1 AND 64),
    capacity_grams    integer NOT NULL CHECK (capacity_grams > 0),
    home_latitude     double precision NOT NULL CHECK (home_latitude BETWEEN -90 AND 90),
    home_longitude    double precision NOT NULL CHECK (home_longitude BETWEEN -180 AND 180),
    cruise_speed_mps  double precision NOT NULL CHECK (cruise_speed_mps > 0 AND cruise_speed_mps <= 100),
    max_altitude_m    double precision NOT NULL DEFAULT 30 CHECK (max_altitude_m > 0 AND max_altitude_m <= 120),
    active            boolean NOT NULL DEFAULT true,
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS telemetry_drone_received_idx
    ON neurovector.telemetry (drone_id, received_at DESC, ts DESC, id DESC);

ALTER TABLE neurovector.deliveries
    ADD COLUMN IF NOT EXISTS estimated_weight_grams bigint CHECK (estimated_weight_grams IS NULL OR estimated_weight_grams > 0),
    ADD COLUMN IF NOT EXISTS schedule_source text NOT NULL DEFAULT 'manual'
        CHECK (schedule_source IN ('manual', 'automatic'));

CREATE TABLE IF NOT EXISTS neurovector.delivery_stops (
    id                    uuid PRIMARY KEY DEFAULT uuid_generate_v4(),
    delivery_id           uuid NOT NULL REFERENCES neurovector.deliveries(id) ON DELETE CASCADE,
    order_id              uuid NOT NULL REFERENCES neurovector.orders(id) ON DELETE CASCADE,
    stop_sequence         integer NOT NULL CHECK (stop_sequence > 0),
    status                text NOT NULL DEFAULT 'scheduled'
        CHECK (status IN ('scheduled', 'arrived', 'completed', 'cancelled')),
    estimated_weight_grams integer NOT NULL CHECK (estimated_weight_grams > 0),
    eta_at                timestamptz NOT NULL,
    arrived_at            timestamptz,
    completed_at          timestamptz,
    created_at            timestamptz NOT NULL DEFAULT now(),
    updated_at            timestamptz NOT NULL DEFAULT now(),
    UNIQUE (delivery_id, stop_sequence)
);

CREATE INDEX IF NOT EXISTS delivery_stops_delivery_status_idx
    ON neurovector.delivery_stops (delivery_id, status, stop_sequence);
CREATE UNIQUE INDEX IF NOT EXISTS delivery_stops_one_active_order_idx
    ON neurovector.delivery_stops (order_id)
    WHERE status IN ('scheduled', 'arrived');

CREATE TABLE IF NOT EXISTS neurovector.delivery_weight_measurements (
    id                       uuid PRIMARY KEY DEFAULT uuid_generate_v4(),
    delivery_id              uuid NOT NULL REFERENCES neurovector.deliveries(id) ON DELETE CASCADE,
    drone_id                 text NOT NULL REFERENCES neurovector.delivery_fleet(drone_id) ON DELETE RESTRICT,
    weight_grams             integer NOT NULL CHECK (weight_grams >= 0),
    capacity_exceeded        boolean NOT NULL,
    measured_by              text NOT NULL,
    measured_at              timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS delivery_weight_measurements_latest_idx
    ON neurovector.delivery_weight_measurements (delivery_id, measured_at DESC);

CREATE TABLE IF NOT EXISTS neurovector.order_tracking_events (
    id              uuid PRIMARY KEY DEFAULT uuid_generate_v4(),
    order_id        uuid NOT NULL REFERENCES neurovector.orders(id) ON DELETE CASCADE,
    delivery_stop_id uuid REFERENCES neurovector.delivery_stops(id) ON DELETE CASCADE,
    event_type      text NOT NULL CHECK (length(btrim(event_type)) BETWEEN 1 AND 80),
    dedupe_key      text,
    payload         jsonb NOT NULL DEFAULT '{}'::jsonb,
    occurred_at     timestamptz NOT NULL DEFAULT now(),
    created_at      timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS order_tracking_events_order_created_idx
    ON neurovector.order_tracking_events (order_id, occurred_at, id);
CREATE UNIQUE INDEX IF NOT EXISTS order_tracking_events_order_id_dedupe_key_key
    ON neurovector.order_tracking_events (order_id, dedupe_key);

CREATE OR REPLACE FUNCTION neurovector.sync_delivery_stop_from_order() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    active_stops integer;
    delivery_status text;
BEGIN
    IF NEW.delivery_id IS NULL OR NEW.status = OLD.status THEN
        RETURN NEW;
    END IF;

    IF NEW.status = 'cancelled' THEN
        SELECT status INTO delivery_status
        FROM neurovector.deliveries
        WHERE id = NEW.delivery_id
        FOR UPDATE;

        IF delivery_status = 'scheduled' THEN
            INSERT INTO neurovector.order_tracking_events
                (order_id, delivery_stop_id, event_type, dedupe_key, payload)
            SELECT s.order_id, s.id, 'delivery_released',
                   'delivery_released:' || NEW.delivery_id::text,
                   jsonb_build_object('delivery_id', NEW.delivery_id, 'reason', 'stop_cancelled')
            FROM neurovector.delivery_stops s
            WHERE s.delivery_id = NEW.delivery_id AND s.order_id <> NEW.id
            ON CONFLICT (order_id, dedupe_key) DO NOTHING;

            UPDATE neurovector.delivery_stops
            SET status = 'cancelled', updated_at = now()
            WHERE delivery_id = NEW.delivery_id AND status IN ('scheduled', 'arrived');
            UPDATE neurovector.orders
            SET delivery_id = NULL,
                status = CASE WHEN status = 'assigned' THEN 'ready' ELSE status END,
                updated_at = now()
            WHERE delivery_id = NEW.delivery_id AND id <> NEW.id
              AND status IN ('paid', 'ready', 'assigned');
            UPDATE neurovector.deliveries
            SET status = 'cancelled', completed_at = COALESCE(completed_at, now()), updated_at = now()
            WHERE id = NEW.delivery_id AND status = 'scheduled';
        ELSE
            UPDATE neurovector.delivery_stops
            SET status = 'cancelled', updated_at = now()
            WHERE order_id = NEW.id AND status IN ('scheduled', 'arrived');
        END IF;

        INSERT INTO neurovector.order_tracking_events (order_id, delivery_stop_id, event_type, dedupe_key, payload)
        SELECT NEW.id,
               (SELECT id FROM neurovector.delivery_stops WHERE order_id = NEW.id ORDER BY created_at DESC LIMIT 1),
               'order_cancelled', 'order_cancelled', jsonb_build_object('status', NEW.status)
        ON CONFLICT (order_id, dedupe_key) DO NOTHING;

    ELSIF NEW.status = 'delivered' THEN
        UPDATE neurovector.delivery_stops
        SET status = 'completed', completed_at = COALESCE(completed_at, now()), updated_at = now()
        WHERE order_id = NEW.id AND status IN ('scheduled', 'arrived');
        INSERT INTO neurovector.order_tracking_events (order_id, delivery_stop_id, event_type, dedupe_key, payload)
        SELECT NEW.id,
               (SELECT id FROM neurovector.delivery_stops WHERE order_id = NEW.id AND status = 'completed' ORDER BY updated_at DESC LIMIT 1),
               'delivered', 'delivered', jsonb_build_object('status', NEW.status)
        ON CONFLICT (order_id, dedupe_key) DO NOTHING;

        SELECT count(*) INTO active_stops
        FROM neurovector.delivery_stops
        WHERE delivery_id = NEW.delivery_id AND status IN ('scheduled', 'arrived');
        IF active_stops = 0 THEN
            UPDATE neurovector.deliveries
            SET status = 'completed', completed_at = COALESCE(completed_at, now()), updated_at = now()
            WHERE id = NEW.delivery_id AND status IN ('dispatched', 'in_flight');
        END IF;
    END IF;

    RETURN NEW;
END $$;

DROP TRIGGER IF EXISTS order_delivery_stop_sync ON neurovector.orders;
CREATE TRIGGER order_delivery_stop_sync
    AFTER UPDATE OF status ON neurovector.orders
    FOR EACH ROW EXECUTE FUNCTION neurovector.sync_delivery_stop_from_order();

CREATE OR REPLACE FUNCTION neurovector.guard_delivery_stop_dispatch() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    fleet_capacity integer;
    fleet_active boolean;
    latest_weight integer;
    latest_measured_at timestamptz;
    telemetry_ttl_seconds double precision;
BEGIN
    telemetry_ttl_seconds := COALESCE(
        NULLIF(current_setting('orca.telemetry_online_ttl_seconds', true), '')::double precision,
        120.0
    );

    IF NEW.status IN ('dispatched', 'in_flight') AND EXISTS (
        SELECT 1
        FROM neurovector.delivery_stops s
        JOIN neurovector.orders o ON o.id = s.order_id
        WHERE s.delivery_id = NEW.id
          AND s.status IN ('scheduled', 'arrived')
          AND o.status NOT IN ('assigned', 'in_transit', 'delivered')
    ) THEN
        RAISE EXCEPTION 'all scheduled delivery orders must be ready before dispatch';
    END IF;

    IF NEW.status IN ('dispatched', 'in_flight') THEN
        SELECT f.capacity_grams, f.active
        INTO fleet_capacity, fleet_active
        FROM neurovector.delivery_fleet f
        WHERE f.drone_id = NEW.drone_id;

        IF FOUND THEN
            IF NOT fleet_active THEN
                RAISE EXCEPTION 'registered fleet drone is inactive';
            END IF;

            IF NOT EXISTS (
                SELECT 1
                FROM (
                    SELECT t.ts, t.received_at, t.status
                    FROM neurovector.telemetry t
                    WHERE t.drone_id = NEW.drone_id
                    ORDER BY t.received_at DESC, t.ts DESC, t.id DESC
                    LIMIT 1
                ) latest
                WHERE latest.ts >= now() - make_interval(secs => telemetry_ttl_seconds)
                  AND latest.ts <= now() + interval '5 seconds'
                  AND latest.received_at >= now() - make_interval(secs => telemetry_ttl_seconds)
                  AND latest.received_at <= now() + interval '5 seconds'
                  AND lower(COALESCE(latest.status, '')) NOT IN ('failed', 'aborted')
            ) THEN
                RAISE EXCEPTION 'registered fleet drone requires fresh live telemetry';
            END IF;

            SELECT m.weight_grams, m.measured_at
            INTO latest_weight, latest_measured_at
            FROM neurovector.delivery_weight_measurements m
            WHERE m.delivery_id = NEW.id
            ORDER BY m.measured_at DESC, m.id DESC
            LIMIT 1;

            IF NEW.schedule_source = 'automatic'
               AND (latest_weight IS NULL OR latest_weight <= 0
                    OR latest_measured_at < now() - interval '5 minutes'
                    OR latest_measured_at > now() + interval '5 seconds') THEN
                RAISE EXCEPTION 'automatic delivery requires a recent positive measured payload';
            END IF;

            IF latest_weight IS NOT NULL AND latest_weight > fleet_capacity THEN
                RAISE EXCEPTION 'latest measured payload exceeds current drone capacity';
            END IF;
        ELSIF NEW.schedule_source = 'automatic' THEN
            RAISE EXCEPTION 'automatic delivery requires an active registered fleet drone';
        END IF;
    END IF;
    RETURN NEW;
END $$;

DROP TRIGGER IF EXISTS delivery_stop_dispatch_guard ON neurovector.deliveries;
CREATE TRIGGER delivery_stop_dispatch_guard
    BEFORE UPDATE OF status ON neurovector.deliveries
    FOR EACH ROW EXECUTE FUNCTION neurovector.guard_delivery_stop_dispatch();
