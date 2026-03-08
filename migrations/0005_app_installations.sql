-- App installations: track devices independently of VPN peers.

CREATE TABLE IF NOT EXISTS app_installations (
    id BIGSERIAL PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    device_id TEXT NOT NULL,
    device_name TEXT,
    platform TEXT,
    app_version TEXT,
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(user_id, device_id)
);

CREATE INDEX IF NOT EXISTS idx_app_installations_user_id ON app_installations(user_id);

-- Migrate existing data from peers into app_installations
INSERT INTO app_installations (user_id, device_id, device_name, app_version)
SELECT DISTINCT ON (user_id, device_id) user_id, device_id, device_name, client_version
FROM peers
WHERE device_id IS NOT NULL
ORDER BY user_id, device_id, client_version DESC NULLS LAST
ON CONFLICT DO NOTHING;

-- Add installation_id FK to peers
ALTER TABLE peers ADD COLUMN IF NOT EXISTS installation_id BIGINT REFERENCES app_installations(id);

-- Backfill installation_id from matching (user_id, device_id)
UPDATE peers p
SET installation_id = ai.id
FROM app_installations ai
WHERE p.user_id = ai.user_id
  AND p.device_id IS NOT NULL
  AND p.device_id = ai.device_id;

-- Drop old index on peers.device_id (replaced by installation_id)
DROP INDEX IF EXISTS idx_peers_device_id_active;

-- Drop old device columns from peers
ALTER TABLE peers DROP COLUMN IF EXISTS device_name;
ALTER TABLE peers DROP COLUMN IF EXISTS device_id;
ALTER TABLE peers DROP COLUMN IF EXISTS client_version;
