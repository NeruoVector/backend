-- 관리자 계정 / 세션 기반 로그인
-- 비밀번호는 Argon2 해시로 저장하고, 세션 토큰은 해시만 DB 에 보관합니다.

CREATE TABLE IF NOT EXISTS neurovector.admin_users (
    id             uuid PRIMARY KEY DEFAULT uuid_generate_v4(),
    username       text        NOT NULL UNIQUE CHECK (length(username) > 0),
    password_hash  text        NOT NULL,
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS neurovector.admin_sessions (
    id             uuid PRIMARY KEY DEFAULT uuid_generate_v4(),
    session_hash   text        NOT NULL UNIQUE,
    user_id        uuid        NOT NULL REFERENCES neurovector.admin_users(id) ON DELETE CASCADE,
    expires_at     timestamptz NOT NULL,
    created_at     timestamptz NOT NULL DEFAULT now(),
    last_seen_at   timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS admin_sessions_user_idx
    ON neurovector.admin_sessions (user_id);
CREATE INDEX IF NOT EXISTS admin_sessions_expires_idx
    ON neurovector.admin_sessions (expires_at);
