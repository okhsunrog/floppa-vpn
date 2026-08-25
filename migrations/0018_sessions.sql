-- Revocable, per-device API sessions.
--
-- Every JWT issued from now on carries a `jti` that is the primary key of a row here; the auth
-- extractor rejects a token whose row is missing or revoked. Tokens issued before this
-- migration carry no `jti` and stay valid until they expire, unless `users.tokens_valid_after`
-- is set: a legacy token issued before that instant is rejected ("log out everywhere" for the
-- tokens that have no row to revoke).
--
-- Numbered 0018 on purpose: 0017 was left free for a migration developed in parallel.

CREATE TABLE IF NOT EXISTS sessions (
    id              UUID PRIMARY KEY,
    user_id         BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- The app installation (device) that reported itself while holding this session.
    installation_id BIGINT REFERENCES app_installations(id) ON DELETE SET NULL,
    -- Which login path minted the session; `legacy` marks a row created when a pre-session
    -- token was refreshed, so an active old client migrates without re-login.
    kind            TEXT NOT NULL CHECK (kind IN (
                        'telegram_widget', 'mini_app', 'deep_link', 'credential', 'legacy'
                    )),
    -- Human-readable device description, filled in when the installation binds.
    label           TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- Bumped at most once an hour by authenticated requests.
    last_seen_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    revoked_at      TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_sessions_user_id ON sessions(user_id);
CREATE INDEX IF NOT EXISTS idx_sessions_installation_id
    ON sessions(installation_id) WHERE installation_id IS NOT NULL;

ALTER TABLE users ADD COLUMN IF NOT EXISTS tokens_valid_after TIMESTAMPTZ;
