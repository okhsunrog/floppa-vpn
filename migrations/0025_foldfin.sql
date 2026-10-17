-- Foldfin has opaque per-installation credentials, not WireGuard key pairs.
-- Node metadata is written by deployment; the API and node read the same policy.
CREATE TABLE foldfin_nodes (
    region_id TEXT PRIMARY KEY REFERENCES regions(id) ON DELETE RESTRICT,
    config JSONB NOT NULL,
    revision BIGINT NOT NULL DEFAULT 1 CHECK (revision > 0),
    enabled BOOLEAN NOT NULL DEFAULT true,
    last_sync_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE foldfin_devices (
    id BIGSERIAL PRIMARY KEY,
    installation_id BIGINT NOT NULL REFERENCES app_installations(id) ON DELETE CASCADE,
    region_id TEXT NOT NULL REFERENCES foldfin_nodes(region_id) ON DELETE RESTRICT,
    slot INT NOT NULL CHECK (slot BETWEEN 2 AND 65534),
    token_digest BYTEA NOT NULL UNIQUE CHECK (octet_length(token_digest) = 32),
    revision BIGINT NOT NULL DEFAULT 1 CHECK (revision > 0),
    applied_revision BIGINT NOT NULL DEFAULT 0 CHECK (applied_revision >= 0),
    issued_by_session UUID REFERENCES sessions(id) ON DELETE SET NULL,
    revoked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (installation_id, region_id),
    UNIQUE (region_id, slot)
);

CREATE FUNCTION notify_foldfin_changed() RETURNS TRIGGER AS $$
BEGIN
    PERFORM pg_notify('foldfin_changed', '');
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER foldfin_devices_changed
    AFTER INSERT OR DELETE OR UPDATE OF token_digest, revision, revoked_at, issued_by_session
    ON foldfin_devices FOR EACH STATEMENT EXECUTE FUNCTION notify_foldfin_changed();
CREATE TRIGGER foldfin_nodes_changed
    AFTER INSERT OR DELETE OR UPDATE OF config, revision, enabled ON foldfin_nodes
    FOR EACH STATEMENT EXECUTE FUNCTION notify_foldfin_changed();
CREATE TRIGGER foldfin_regions_changed
    AFTER INSERT OR UPDATE OR DELETE ON plan_regions
    FOR EACH STATEMENT EXECUTE FUNCTION notify_foldfin_changed();
CREATE TRIGGER foldfin_region_state_changed
    AFTER UPDATE ON regions FOR EACH STATEMENT EXECUTE FUNCTION notify_foldfin_changed();
CREATE TRIGGER foldfin_private_networks_changed
    AFTER INSERT OR UPDATE OR DELETE ON private_networks
    FOR EACH STATEMENT EXECUTE FUNCTION notify_foldfin_changed();
CREATE TRIGGER foldfin_private_grants_changed
    AFTER INSERT OR UPDATE OR DELETE ON plan_private_networks
    FOR EACH STATEMENT EXECUTE FUNCTION notify_foldfin_changed();

-- API logout and device region changes must not leave an old VPN token usable.
CREATE FUNCTION revoke_foldfin_session() RETURNS TRIGGER AS $$
BEGIN
    IF OLD.revoked_at IS NULL AND NEW.revoked_at IS NOT NULL THEN
        UPDATE foldfin_devices SET revoked_at = NOW(), revision = revision + 1
        WHERE issued_by_session = NEW.id AND revoked_at IS NULL;
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER foldfin_session_revoked AFTER UPDATE OF revoked_at ON sessions
    FOR EACH ROW EXECUTE FUNCTION revoke_foldfin_session();

CREATE FUNCTION revoke_foldfin_region() RETURNS TRIGGER AS $$
BEGIN
    IF OLD.region_id IS DISTINCT FROM NEW.region_id THEN
        UPDATE foldfin_devices SET revoked_at = NOW(), revision = revision + 1
        WHERE installation_id = NEW.id AND revoked_at IS NULL;
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER foldfin_installation_region_changed AFTER UPDATE OF region_id ON app_installations
    FOR EACH ROW EXECUTE FUNCTION revoke_foldfin_region();

-- One device slot across WG/AWG/Foldfin; exported WG configurations retain their own slots.
CREATE VIEW vpn_device_slots AS
    SELECT user_id, installation_id FROM peers
    WHERE sync_status <> 'removed' AND installation_id IS NOT NULL
    UNION
    SELECT ai.user_id, fd.installation_id
    FROM foldfin_devices fd JOIN app_installations ai ON ai.id = fd.installation_id
    WHERE fd.revoked_at IS NULL;
