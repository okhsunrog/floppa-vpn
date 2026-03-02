ALTER TABLE peers ADD COLUMN device_name TEXT;
ALTER TABLE peers ADD COLUMN device_id TEXT;

CREATE UNIQUE INDEX idx_peers_device_id_active
    ON peers (device_id)
    WHERE device_id IS NOT NULL AND sync_status NOT IN ('removed', 'pending_remove');
