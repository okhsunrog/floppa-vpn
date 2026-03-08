-- Move VLESS from peers to users (per-user UUID model).

-- 1. Add vless_uuid to users (generated on demand)
ALTER TABLE users ADD COLUMN IF NOT EXISTS vless_uuid TEXT;
CREATE UNIQUE INDEX IF NOT EXISTS idx_users_vless_uuid ON users(vless_uuid) WHERE vless_uuid IS NOT NULL;

-- 2. Add VLESS traffic columns to users
ALTER TABLE users ADD COLUMN IF NOT EXISTS vless_tx_bytes BIGINT NOT NULL DEFAULT 0;
ALTER TABLE users ADD COLUMN IF NOT EXISTS vless_rx_bytes BIGINT NOT NULL DEFAULT 0;

-- 3. Clean up VLESS peers
DELETE FROM peers WHERE protocol = 'vless';

-- 4. Remove VLESS columns/indexes from peers
DROP INDEX IF EXISTS idx_peers_vless_uuid_active;
DROP INDEX IF EXISTS idx_peers_protocol;
ALTER TABLE peers DROP COLUMN IF EXISTS vless_uuid;
ALTER TABLE peers DROP COLUMN IF EXISTS protocol;

-- 5. Restore NOT NULL on WG-only columns
ALTER TABLE peers ALTER COLUMN public_key SET NOT NULL;
ALTER TABLE peers ALTER COLUMN assigned_ip SET NOT NULL;

-- 6. Recreate device_id unique index (no protocol dimension)
DROP INDEX IF EXISTS idx_peers_device_id_active;
CREATE UNIQUE INDEX idx_peers_device_id_active
    ON peers (device_id)
    WHERE device_id IS NOT NULL
    AND sync_status NOT IN ('removed', 'pending_remove');

-- 7. Trigger: notify floppa-vless when user's vless_uuid changes
CREATE OR REPLACE FUNCTION notify_user_vless_changed() RETURNS TRIGGER AS $$
BEGIN
    IF OLD.vless_uuid IS DISTINCT FROM NEW.vless_uuid THEN
        PERFORM pg_notify('vless_user_changed', NEW.id::text);
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS user_vless_changed_trigger ON users;
CREATE TRIGGER user_vless_changed_trigger
    AFTER UPDATE ON users
    FOR EACH ROW
    EXECUTE FUNCTION notify_user_vless_changed();
