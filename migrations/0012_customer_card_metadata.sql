-- Display-only card notes: never PAN, CVC, expiry or payment tokens.
CREATE TABLE IF NOT EXISTS neurovector.customer_card_metadata (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES neurovector.customer_users(id) ON DELETE CASCADE,
    ciphertext bytea NOT NULL CHECK (octet_length(ciphertext) >= 16),
    nonce bytea NOT NULL CHECK (octet_length(nonce) = 12),
    key_id text NOT NULL,
    format_version smallint NOT NULL CHECK (format_version = 1),
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS customer_card_metadata_user_idx ON neurovector.customer_card_metadata(user_id,created_at);
