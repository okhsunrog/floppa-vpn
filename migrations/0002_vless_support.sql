-- Add VLESS protocol support to the peers table.

-- Protocol column: "wireguard" or "vless"
ALTER TABLE peers ADD COLUMN IF NOT EXISTS protocol TEXT NOT NULL DEFAULT 'wireguard';

-- VLESS UUID (NULL for WireGuard peers)
ALTER TABLE peers ADD COLUMN IF NOT EXISTS vless_uuid TEXT;

-- WireGuard-specific columns must be nullable for VLESS peers
ALTER TABLE peers ALTER COLUMN public_key DROP NOT NULL;
ALTER TABLE peers ALTER COLUMN assigned_ip DROP NOT NULL;

-- Replace device_id unique index: one active peer per (device_id, protocol)
DROP INDEX IF EXISTS idx_peers_device_id_active;
CREATE UNIQUE INDEX idx_peers_device_id_active
    ON peers (device_id, protocol)
    WHERE device_id IS NOT NULL
    AND sync_status NOT IN ('removed', 'pending_remove');

-- VLESS UUID must be unique among active peers
CREATE UNIQUE INDEX IF NOT EXISTS idx_peers_vless_uuid_active
    ON peers (vless_uuid)
    WHERE vless_uuid IS NOT NULL
    AND sync_status NOT IN ('removed', 'pending_remove');

-- Index for fast VLESS peer lookups by protocol
CREATE INDEX IF NOT EXISTS idx_peers_protocol ON peers(protocol);
