-- Legacy data did not record whether a frame advanced current state. Conservatively
-- backfill false: preserve every historical row, but require a fresh new sample to
-- establish authority and prevent legacy replays from fabricating arrival history.
ALTER TABLE neurovector.telemetry
    ADD COLUMN IF NOT EXISTS accepted_for_current_state boolean NOT NULL DEFAULT false;

CREATE INDEX IF NOT EXISTS telemetry_current_drone_idx
    ON neurovector.telemetry (drone_id, ts DESC, id ASC)
    WHERE accepted_for_current_state;

CREATE OR REPLACE VIEW neurovector.current_drone_telemetry AS
SELECT DISTINCT ON (drone_id) *
FROM neurovector.telemetry
WHERE accepted_for_current_state
ORDER BY drone_id, ts DESC, id ASC;

-- Replace the dispatch trigger function without changing its other guards.
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
                    FROM neurovector.current_drone_telemetry t
                    WHERE t.drone_id = NEW.drone_id
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
