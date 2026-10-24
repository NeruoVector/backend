-- Existing audit events remain historical; new events are delivered by the outbox worker.
ALTER TABLE neurovector.order_events
    ADD COLUMN IF NOT EXISTS published_at timestamptz DEFAULT now();
ALTER TABLE neurovector.order_events ALTER COLUMN published_at DROP DEFAULT;

CREATE INDEX IF NOT EXISTS order_events_unpublished_idx
    ON neurovector.order_events (created_at)
    WHERE published_at IS NULL;
