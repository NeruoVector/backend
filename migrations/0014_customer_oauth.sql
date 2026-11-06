-- Missing provider phone data stays empty; real phone numbers remain unique.
ALTER TABLE neurovector.customer_users DROP CONSTRAINT IF EXISTS customer_users_phone_key;
CREATE UNIQUE INDEX IF NOT EXISTS customer_users_nonempty_phone_key
    ON neurovector.customer_users(phone) WHERE phone <> '';

CREATE TABLE IF NOT EXISTS neurovector.customer_oauth_identities (
    provider text NOT NULL CHECK (provider IN ('kakao','naver','google')),
    subject text NOT NULL CHECK (length(subject) BETWEEN 1 AND 255),
    user_id uuid NOT NULL REFERENCES neurovector.customer_users(id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY(provider, subject),
    UNIQUE(provider, user_id)
);
CREATE TABLE IF NOT EXISTS neurovector.customer_oauth_flows (
    state_hash text PRIMARY KEY,
    provider text NOT NULL CHECK (provider IN ('kakao','naver','google')),
    verifier text NOT NULL,
    expires_at timestamptz NOT NULL
);
CREATE INDEX IF NOT EXISTS customer_oauth_flows_expiry_idx ON neurovector.customer_oauth_flows(expires_at);
