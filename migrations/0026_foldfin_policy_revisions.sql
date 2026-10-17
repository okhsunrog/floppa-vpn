-- Expose policy changes to clients through the same monotonic configuration revision.
-- Extending expiry alone preserves the active lease and does not change the document.
CREATE FUNCTION bump_foldfin_plan_policy() RETURNS TRIGGER AS $$
DECLARE
    affected_plans INT[];
BEGIN
    IF TG_TABLE_NAME = 'plans' THEN
        IF OLD.default_speed_limit_mbps IS NOT DISTINCT FROM NEW.default_speed_limit_mbps THEN
            RETURN NULL;
        END IF;
        affected_plans := ARRAY[NEW.id];
    ELSIF TG_TABLE_NAME = 'plan_private_networks' THEN
        IF TG_OP = 'INSERT' THEN affected_plans := ARRAY[NEW.plan_id];
        ELSIF TG_OP = 'DELETE' THEN affected_plans := ARRAY[OLD.plan_id];
        ELSE affected_plans := ARRAY[OLD.plan_id, NEW.plan_id];
        END IF;
    ELSE
        IF OLD.cidrs IS NOT DISTINCT FROM NEW.cidrs AND OLD.is_active IS NOT DISTINCT FROM NEW.is_active THEN
            RETURN NULL;
        END IF;
        SELECT ARRAY_AGG(plan_id) INTO affected_plans FROM plan_private_networks WHERE network_id=NEW.id;
    END IF;
    UPDATE foldfin_devices d SET revision=d.revision+1
    FROM app_installations ai, current_subscriptions cs
    WHERE ai.id=d.installation_id AND cs.user_id=ai.user_id
      AND cs.plan_id=ANY(affected_plans) AND d.revoked_at IS NULL;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER foldfin_plan_speed_revision AFTER UPDATE OF default_speed_limit_mbps ON plans
    FOR EACH ROW EXECUTE FUNCTION bump_foldfin_plan_policy();
CREATE TRIGGER foldfin_private_grant_revision AFTER INSERT OR UPDATE OR DELETE ON plan_private_networks
    FOR EACH ROW EXECUTE FUNCTION bump_foldfin_plan_policy();
CREATE TRIGGER foldfin_private_network_revision AFTER UPDATE OF cidrs,is_active ON private_networks
    FOR EACH ROW EXECUTE FUNCTION bump_foldfin_plan_policy();

CREATE FUNCTION bump_foldfin_subscription_policy() RETURNS TRIGGER AS $$
DECLARE previous_plan INT;
BEGIN
    IF NOT NEW.is_current THEN RETURN NULL; END IF;
    IF TG_OP = 'INSERT' THEN
        SELECT plan_id INTO previous_plan FROM subscriptions
        WHERE user_id=NEW.user_id AND id<>NEW.id ORDER BY id DESC LIMIT 1;
        IF previous_plan IS NOT DISTINCT FROM NEW.plan_id THEN RETURN NULL; END IF;
    ELSIF OLD.plan_id IS NOT DISTINCT FROM NEW.plan_id AND OLD.is_current THEN
        RETURN NULL;
    END IF;
    UPDATE foldfin_devices d SET revision=d.revision+1 FROM app_installations ai
    WHERE ai.id=d.installation_id AND ai.user_id=NEW.user_id AND d.revoked_at IS NULL;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER foldfin_subscription_policy_revision AFTER INSERT OR UPDATE OF plan_id,is_current ON subscriptions
    FOR EACH ROW EXECUTE FUNCTION bump_foldfin_subscription_policy();
