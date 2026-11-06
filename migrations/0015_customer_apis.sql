CREATE TABLE IF NOT EXISTS neurovector.users (
    id uuid PRIMARY KEY DEFAULT uuid_generate_v4(),
    username text NOT NULL UNIQUE CHECK (length(username) BETWEEN 3 AND 80),
    password_hash text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE IF NOT EXISTS neurovector.user_sessions (
    session_hash text PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES neurovector.users(id) ON DELETE CASCADE,
    expires_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS user_sessions_expiry_idx ON neurovector.user_sessions(expires_at);
CREATE INDEX IF NOT EXISTS user_sessions_user_idx ON neurovector.user_sessions(user_id);

CREATE TABLE IF NOT EXISTS neurovector.schools (
    code text PRIMARY KEY CHECK (length(btrim(code)) BETWEEN 1 AND 40),
    name text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 200),
    region text NOT NULL CHECK (length(btrim(region)) BETWEEN 1 AND 100)
);
CREATE TABLE IF NOT EXISTS neurovector.reviews (
    id uuid PRIMARY KEY DEFAULT uuid_generate_v4(),
    user_id uuid NOT NULL REFERENCES neurovector.users(id) ON DELETE CASCADE,
    item_id uuid NOT NULL REFERENCES neurovector.inventory_items(id) ON DELETE CASCADE,
    rating smallint NOT NULL CHECK (rating BETWEEN 1 AND 5),
    content text NOT NULL CHECK (length(btrim(content)) BETWEEN 1 AND 2000),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE(user_id, item_id)
);
CREATE INDEX IF NOT EXISTS reviews_item_idx ON neurovector.reviews(item_id, created_at DESC);
CREATE TABLE IF NOT EXISTS neurovector.notifications (
    id uuid PRIMARY KEY DEFAULT uuid_generate_v4(),
    user_id uuid NOT NULL REFERENCES neurovector.users(id) ON DELETE CASCADE,
    title text NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 100),
    body text NOT NULL CHECK (length(btrim(body)) BETWEEN 1 AND 4000),
    created_at timestamptz NOT NULL DEFAULT now(),
    read_at timestamptz
);
CREATE INDEX IF NOT EXISTS notifications_user_idx ON neurovector.notifications(user_id, created_at DESC);
