-- The selected exit region belongs to an installation and is copied to each
-- WireGuard-family peer so floppa-daemon can route by the peer's source IP.

ALTER TABLE regions
    ADD COLUMN supports_vless BOOLEAN NOT NULL DEFAULT false;

UPDATE regions SET supports_vless = (id = 'europe');

ALTER TABLE app_installations
    ADD COLUMN region_id TEXT NOT NULL DEFAULT 'europe'
    REFERENCES regions(id) ON DELETE RESTRICT;

ALTER TABLE peers
    ADD COLUMN region_id TEXT NOT NULL DEFAULT 'europe'
    REFERENCES regions(id) ON DELETE RESTRICT;

CREATE INDEX idx_peers_region_active ON peers(region_id)
WHERE sync_status NOT IN ('removed', 'pending_remove');

CREATE OR REPLACE FUNCTION notify_peer_changed()
RETURNS TRIGGER AS $$
BEGIN
    IF TG_OP = 'INSERT' OR
       (TG_OP = 'UPDATE' AND (
           OLD.sync_status IS DISTINCT FROM NEW.sync_status OR
           OLD.region_id IS DISTINCT FROM NEW.region_id
       )) THEN
        PERFORM pg_notify('peer_changed', NEW.id::text);
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
