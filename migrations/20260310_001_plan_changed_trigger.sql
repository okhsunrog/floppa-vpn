-- Notify daemon when a plan's speed limit changes, so existing subscribers get updated.
-- Reuses the 'subscription_changed' channel, sending one notification per affected user.

CREATE OR REPLACE FUNCTION notify_plan_changed() RETURNS trigger AS $$
DECLARE
    affected_user_id BIGINT;
BEGIN
    -- Only fire when speed limit actually changes
    IF TG_OP = 'UPDATE' AND
       OLD.default_speed_limit_mbps IS NOT DISTINCT FROM NEW.default_speed_limit_mbps
    THEN
        RETURN NEW;
    END IF;

    -- Notify for each user with an active subscription on this plan
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

CREATE TRIGGER plan_changed_trigger
    AFTER UPDATE ON plans
    FOR EACH ROW
    EXECUTE FUNCTION notify_plan_changed();
