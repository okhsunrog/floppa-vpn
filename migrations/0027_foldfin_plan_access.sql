-- Opt-in protocol rollout. Existing and newly created plans deny Foldfin by default.
ALTER TABLE plans ADD COLUMN foldfin_enabled BOOLEAN NOT NULL DEFAULT false;
UPDATE plans SET foldfin_enabled = true WHERE name = 'admin';

-- Enforce the initial rollout even before every regional node has been upgraded.
UPDATE foldfin_devices d SET revoked_at=NOW(), revision=d.revision+1
WHERE d.revoked_at IS NULL AND NOT EXISTS (
    SELECT 1 FROM app_installations ai
    JOIN current_subscriptions cs ON cs.user_id=ai.user_id AND cs.is_active
    JOIN plans p ON p.id=cs.plan_id AND p.foldfin_enabled
    WHERE ai.id=d.installation_id
);

CREATE FUNCTION apply_foldfin_plan_access() RETURNS TRIGGER AS $$
BEGIN
    IF OLD.foldfin_enabled IS DISTINCT FROM NEW.foldfin_enabled THEN
        IF NOT NEW.foldfin_enabled THEN
            UPDATE foldfin_devices d SET revoked_at=NOW(), revision=d.revision+1
            FROM app_installations ai, current_subscriptions cs
            WHERE ai.id=d.installation_id AND cs.user_id=ai.user_id
              AND cs.plan_id=NEW.id AND d.revoked_at IS NULL;
        END IF;
        PERFORM pg_notify('foldfin_changed', '');
    END IF;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;
CREATE TRIGGER foldfin_plan_access_changed AFTER UPDATE OF foldfin_enabled ON plans
    FOR EACH ROW EXECUTE FUNCTION apply_foldfin_plan_access();
