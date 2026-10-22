CREATE TABLE IF NOT EXISTS neurovector.mission_plans (
    id uuid PRIMARY KEY,
    route_id uuid NOT NULL UNIQUE REFERENCES neurovector.routes(id) ON DELETE CASCADE,
    drone_id text NOT NULL CHECK (length(drone_id) BETWEEN 1 AND 128),
    status text NOT NULL DEFAULT 'planned' CHECK (status IN ('planned', 'verified', 'rejected')),
    route_fingerprint text NOT NULL,
    plan_text text NOT NULL,
    payload jsonb NOT NULL,
    cross_check jsonb,
    expires_at timestamptz NOT NULL,
    verified_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((status = 'verified') = (verified_at IS NOT NULL)),
    CHECK (expires_at > created_at)
);
CREATE INDEX IF NOT EXISTS mission_plans_drone_created_idx ON neurovector.mission_plans (drone_id, created_at DESC);
