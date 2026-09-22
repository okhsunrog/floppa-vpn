-- Every new plan remains usable without extra administration. Additional exits
-- are explicit grants layered on top of this default.

CREATE OR REPLACE FUNCTION grant_default_plan_region()
RETURNS TRIGGER AS $$
BEGIN
    INSERT INTO plan_regions (plan_id, region_id)
    VALUES (NEW.id, 'europe')
    ON CONFLICT DO NOTHING;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER grant_default_plan_region_after_insert
AFTER INSERT ON plans
FOR EACH ROW EXECUTE FUNCTION grant_default_plan_region();
