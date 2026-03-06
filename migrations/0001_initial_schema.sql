-- Consolidated schema for Floppa VPN.
-- Idempotent: safe to run on an existing database.

-- ============================================================
-- Tables
-- ============================================================

CREATE TABLE IF NOT EXISTS users (
    id BIGSERIAL PRIMARY KEY,
    telegram_id BIGINT UNIQUE NOT NULL,
    username TEXT,
    first_name TEXT,
    last_name TEXT,
    photo_url TEXT,
    is_admin BOOLEAN NOT NULL DEFAULT false,
    language TEXT,
    trial_used_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS plans (
    id SERIAL PRIMARY KEY,
    name TEXT UNIQUE NOT NULL,
    display_name TEXT NOT NULL,
    default_speed_limit_mbps INT,
    default_traffic_limit_bytes BIGINT,
    max_peers INT NOT NULL DEFAULT 1,
    price_rub INT NOT NULL DEFAULT 0,
    is_public BOOLEAN NOT NULL DEFAULT true,
    trial_days INT,
    price_stars INT,
    period_days INT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS subscriptions (
    id BIGSERIAL PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    plan_id INT NOT NULL REFERENCES plans(id),
    starts_at TIMESTAMPTZ NOT NULL,
    expires_at TIMESTAMPTZ,
    payment_id TEXT,
    source TEXT NOT NULL DEFAULT 'admin_grant',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS peers (
    id BIGSERIAL PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    public_key TEXT NOT NULL,
    private_key_encrypted TEXT,
    assigned_ip TEXT NOT NULL,
    sync_status TEXT NOT NULL DEFAULT 'pending_add',
    device_name TEXT,
    device_id TEXT,
    client_version TEXT,
    traffic_used_bytes BIGINT NOT NULL DEFAULT 0,
    tx_bytes BIGINT NOT NULL DEFAULT 0,
    rx_bytes BIGINT NOT NULL DEFAULT 0,
    last_handshake TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS payments (
    id BIGSERIAL PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES users(id),
    plan_id INT NOT NULL REFERENCES plans(id),
    provider TEXT NOT NULL DEFAULT 'telegram_stars',
    currency TEXT NOT NULL DEFAULT 'XTR',
    amount INT NOT NULL,
    credit_amount INT NOT NULL DEFAULT 0,
    status TEXT NOT NULL DEFAULT 'pending',
    telegram_charge_id TEXT UNIQUE,
    invoice_payload TEXT NOT NULL,
    subscription_id BIGINT REFERENCES subscriptions(id),
    provider_data JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ
);

-- ============================================================
-- Indexes
-- ============================================================

CREATE INDEX IF NOT EXISTS idx_peers_sync_status ON peers(sync_status);
CREATE INDEX IF NOT EXISTS idx_peers_user_id ON peers(user_id);

-- Partial unique indexes: removed peers release their IPs/keys for reuse
CREATE UNIQUE INDEX IF NOT EXISTS peers_assigned_ip_active
    ON peers(assigned_ip) WHERE sync_status NOT IN ('removed');
CREATE UNIQUE INDEX IF NOT EXISTS peers_public_key_active
    ON peers(public_key) WHERE sync_status NOT IN ('removed');
CREATE UNIQUE INDEX IF NOT EXISTS idx_peers_device_id_active
    ON peers(device_id) WHERE device_id IS NOT NULL
    AND sync_status NOT IN ('removed', 'pending_remove');

CREATE INDEX IF NOT EXISTS idx_subscriptions_user_id ON subscriptions(user_id);
CREATE INDEX IF NOT EXISTS idx_subscriptions_expires_at ON subscriptions(expires_at);
CREATE INDEX IF NOT EXISTS idx_subscriptions_plan_id ON subscriptions(plan_id);
CREATE INDEX IF NOT EXISTS idx_subscriptions_user_expires
    ON subscriptions(user_id, expires_at DESC NULLS FIRST);

CREATE INDEX IF NOT EXISTS idx_plans_is_public ON plans(is_public);

CREATE INDEX IF NOT EXISTS idx_payments_user_id ON payments(user_id);
CREATE INDEX IF NOT EXISTS idx_payments_status ON payments(status);

-- ============================================================
-- Seed plans (no-op if they already exist)
-- ============================================================

INSERT INTO plans (name, display_name, default_speed_limit_mbps, max_peers, trial_days, is_public, price_rub)
VALUES ('basic', 'Basic', 10, 1, 7, true, 0)
ON CONFLICT (name) DO NOTHING;

INSERT INTO plans (name, display_name, default_speed_limit_mbps, max_peers, is_public, price_rub, period_days)
VALUES ('standard', 'Standard', 50, 3, true, 0, 30)
ON CONFLICT (name) DO NOTHING;

INSERT INTO plans (name, display_name, default_speed_limit_mbps, max_peers, is_public, price_rub, period_days)
VALUES ('premium', 'Premium', 100, 5, true, 0, 30)
ON CONFLICT (name) DO NOTHING;

-- ============================================================
-- Triggers
-- ============================================================

-- Notify daemon when peer sync_status changes
CREATE OR REPLACE FUNCTION notify_peer_changed()
RETURNS TRIGGER AS $$
BEGIN
    IF TG_OP = 'INSERT' OR
       (TG_OP = 'UPDATE' AND OLD.sync_status != NEW.sync_status) THEN
        PERFORM pg_notify('peer_changed', NEW.id::text);
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS peer_changed_trigger ON peers;
CREATE TRIGGER peer_changed_trigger
    AFTER INSERT OR UPDATE ON peers
    FOR EACH ROW
    EXECUTE FUNCTION notify_peer_changed();

-- Notify daemon when subscription changes (plan switch, expiry, new sub)
CREATE OR REPLACE FUNCTION notify_subscription_changed()
RETURNS TRIGGER AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        PERFORM pg_notify('subscription_changed', OLD.user_id::text);
        RETURN OLD;
    END IF;
    IF TG_OP = 'INSERT' OR
       (TG_OP = 'UPDATE' AND (
           OLD.plan_id IS DISTINCT FROM NEW.plan_id OR
           OLD.expires_at IS DISTINCT FROM NEW.expires_at
       ))
    THEN
        PERFORM pg_notify('subscription_changed', NEW.user_id::text);
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS subscription_changed_trigger ON subscriptions;
CREATE TRIGGER subscription_changed_trigger
    AFTER INSERT OR UPDATE OR DELETE ON subscriptions
    FOR EACH ROW
    EXECUTE FUNCTION notify_subscription_changed();

-- Notify daemon when a plan's speed limit changes (propagate to active subscribers)
CREATE OR REPLACE FUNCTION notify_plan_changed() RETURNS TRIGGER AS $$
DECLARE
    affected_user_id BIGINT;
BEGIN
    IF TG_OP = 'UPDATE' AND
       OLD.default_speed_limit_mbps IS NOT DISTINCT FROM NEW.default_speed_limit_mbps
    THEN
        RETURN NEW;
    END IF;

    FOR affected_user_id IN
        SELECT DISTINCT s.user_id
        FROM subscriptions s
        WHERE s.plan_id = NEW.id
          AND (s.expires_at IS NULL OR s.expires_at > NOW())
    LOOP
        PERFORM pg_notify('subscription_changed', affected_user_id::text);
    END LOOP;

    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS plan_changed_trigger ON plans;
CREATE TRIGGER plan_changed_trigger
    AFTER UPDATE ON plans
    FOR EACH ROW
    EXECUTE FUNCTION notify_plan_changed();
