-- Credential auth (login + password): decouple identity from Telegram.
-- Idempotent: safe to run on an existing database.

-- ============================================================
-- 1. telegram_id becomes optional; partial-unique replaces inline UNIQUE.
-- ============================================================
ALTER TABLE users ALTER COLUMN telegram_id DROP NOT NULL;
-- Drop the implicit UNIQUE constraint created by `telegram_id BIGINT UNIQUE` in 0001.
ALTER TABLE users DROP CONSTRAINT IF EXISTS users_telegram_id_key;
CREATE UNIQUE INDEX IF NOT EXISTS users_telegram_id_unique
    ON users(telegram_id) WHERE telegram_id IS NOT NULL;

-- ============================================================
-- 2. Credential identities (NON-telegram logins only).
--    Telegram stays the users.telegram_id column, NOT a row here.
-- ============================================================
CREATE TABLE IF NOT EXISTS auth_identities (
    id            BIGSERIAL PRIMARY KEY,
    user_id       BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider      TEXT   NOT NULL,                 -- v1: 'password'
    provider_uid  TEXT   NOT NULL,                 -- lower(login)
    secret_hash   TEXT,                            -- Argon2id PHC string
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_login_at TIMESTAMPTZ,
    UNIQUE(provider, provider_uid),
    UNIQUE(user_id, provider)
);
CREATE INDEX IF NOT EXISTS idx_auth_identities_user_id ON auth_identities(user_id);

-- ============================================================
-- 3. Dedicated low-limit taster plan (non-public; duration comes from config, not the plan).
-- ============================================================
INSERT INTO plans (name, display_name, default_speed_limit_mbps, max_peers, is_public)
VALUES ('taster', 'Taster', 5, 1, false)
ON CONFLICT (name) DO NOTHING;

-- ============================================================
-- 4. Telegram link codes (read by BOTH the Axum side and the bot, so DB-backed,
--    not in-memory like the existing login-state maps).
-- ============================================================
CREATE TABLE IF NOT EXISTS telegram_link_codes (
    code        TEXT PRIMARY KEY,
    user_id     BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    kind        TEXT,                              -- 'simple' | 'merge' (stamped by bot on consume)
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at  TIMESTAMPTZ NOT NULL,
    consumed_at TIMESTAMPTZ
);
CREATE INDEX IF NOT EXISTS idx_telegram_link_codes_user ON telegram_link_codes(user_id);
