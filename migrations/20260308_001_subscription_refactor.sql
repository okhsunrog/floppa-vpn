-- Subscription refactor: one active subscription per user, no per-sub overrides.
-- Limits come from plans only.

-- 1. Clean up duplicate active subscriptions: keep the best plan per user
WITH ranked AS (
    SELECT s.id, s.user_id,
           ROW_NUMBER() OVER (
               PARTITION BY s.user_id
               ORDER BY p.default_speed_limit_mbps DESC NULLS LAST,
                        s.expires_at DESC NULLS FIRST
           ) AS rn
    FROM subscriptions s
    JOIN plans p ON s.plan_id = p.id
    WHERE s.expires_at IS NULL OR s.expires_at > NOW()
)
UPDATE subscriptions SET expires_at = NOW()
WHERE id IN (SELECT id FROM ranked WHERE rn > 1);

-- 2. Drop per-subscription limit override columns
ALTER TABLE subscriptions DROP COLUMN IF EXISTS speed_limit_mbps;
ALTER TABLE subscriptions DROP COLUMN IF EXISTS traffic_limit_bytes;

-- 3. Rewrite trigger: fire on INSERT, UPDATE (plan_id, expires_at), DELETE
CREATE OR REPLACE FUNCTION notify_subscription_changed()
RETURNS TRIGGER AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        PERFORM pg_notify('subscription_changed', OLD.user_id::text);
        RETURN OLD;
    END IF;
    -- INSERT or UPDATE
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
