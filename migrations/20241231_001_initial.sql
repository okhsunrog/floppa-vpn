-- Initial schema for Floppa VPN

CREATE TABLE users (
    id BIGSERIAL PRIMARY KEY,
    telegram_id BIGINT UNIQUE NOT NULL,
    username TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE peers (
    id BIGSERIAL PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    public_key TEXT UNIQUE NOT NULL,
    private_key_encrypted TEXT,
    assigned_ip TEXT UNIQUE NOT NULL,
    sync_status TEXT NOT NULL DEFAULT 'pending_add',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_handshake TIMESTAMPTZ,
    tx_bytes BIGINT NOT NULL DEFAULT 0,
    rx_bytes BIGINT NOT NULL DEFAULT 0
);

CREATE TABLE subscriptions (
    id BIGSERIAL PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    plan TEXT NOT NULL,
    starts_at TIMESTAMPTZ NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    payment_id TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Indexes for common queries
CREATE INDEX idx_peers_sync_status ON peers(sync_status);
CREATE INDEX idx_peers_user_id ON peers(user_id);
CREATE INDEX idx_subscriptions_user_id ON subscriptions(user_id);
CREATE INDEX idx_subscriptions_expires_at ON subscriptions(expires_at);

-- Trigger to notify daemon when peers change
CREATE OR REPLACE FUNCTION notify_peer_changed()
RETURNS TRIGGER AS $$
BEGIN
    -- Only notify on status changes that daemon cares about
    IF TG_OP = 'INSERT' OR
       (TG_OP = 'UPDATE' AND OLD.sync_status != NEW.sync_status) THEN
        PERFORM pg_notify('peer_changed', NEW.id::text);
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER peer_changed_trigger
    AFTER INSERT OR UPDATE ON peers
    FOR EACH ROW
    EXECUTE FUNCTION notify_peer_changed();
