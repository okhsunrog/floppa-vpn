-- Replace plain UNIQUE constraints on peers with partial unique indexes
-- so that removed peers release their IPs and public keys for reuse.

ALTER TABLE peers DROP CONSTRAINT peers_assigned_ip_key;
ALTER TABLE peers DROP CONSTRAINT peers_public_key_key;

CREATE UNIQUE INDEX peers_assigned_ip_active ON peers(assigned_ip)
    WHERE sync_status NOT IN ('removed');

CREATE UNIQUE INDEX peers_public_key_active ON peers(public_key)
    WHERE sync_status NOT IN ('removed');
