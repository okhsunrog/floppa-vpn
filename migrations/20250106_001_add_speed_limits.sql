-- Add speed limiting support

-- Add speed limit to subscriptions (NULL = unlimited)
ALTER TABLE subscriptions ADD COLUMN speed_limit_mbps INTEGER;

-- Add traffic limit per billing period (NULL = unlimited)
ALTER TABLE subscriptions ADD COLUMN traffic_limit_bytes BIGINT;

-- Track traffic usage per billing period
ALTER TABLE peers ADD COLUMN traffic_used_bytes BIGINT NOT NULL DEFAULT 0;

-- Add index for traffic limit checks
CREATE INDEX idx_subscriptions_speed_limit ON subscriptions(speed_limit_mbps) WHERE speed_limit_mbps IS NOT NULL;

-- Comment on columns
COMMENT ON COLUMN subscriptions.speed_limit_mbps IS 'Bandwidth limit in Mbps (NULL = unlimited)';
COMMENT ON COLUMN subscriptions.traffic_limit_bytes IS 'Monthly traffic limit in bytes (NULL = unlimited)';
COMMENT ON COLUMN peers.traffic_used_bytes IS 'Traffic used in current billing period (reset monthly)';

-- Trigger to notify daemon when subscription speed limit changes
CREATE OR REPLACE FUNCTION notify_subscription_changed()
RETURNS TRIGGER AS $$
BEGIN
    -- Notify when speed limit changes (including NULL <-> value transitions)
    IF TG_OP = 'UPDATE' AND
       (OLD.speed_limit_mbps IS DISTINCT FROM NEW.speed_limit_mbps) THEN
        PERFORM pg_notify('subscription_changed', NEW.user_id::text);
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER subscription_changed_trigger
    AFTER UPDATE ON subscriptions
    FOR EACH ROW
    EXECUTE FUNCTION notify_subscription_changed();
