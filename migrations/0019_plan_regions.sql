-- VPN exit regions and plan entitlements.
--
-- Region access is data, not a hard-coded premium flag: each plan explicitly
-- grants the regions its subscribers may select.

CREATE TABLE IF NOT EXISTS regions (
    id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    sort_order INT NOT NULL DEFAULT 0,
    is_active BOOLEAN NOT NULL DEFAULT true
);

INSERT INTO regions (id, display_name, sort_order)
VALUES
    ('europe', 'Europe', 10),
    ('singapore', 'Singapore', 20)
ON CONFLICT (id) DO UPDATE SET
    display_name = EXCLUDED.display_name,
    sort_order = EXCLUDED.sort_order;

CREATE TABLE IF NOT EXISTS plan_regions (
    plan_id INT NOT NULL REFERENCES plans(id) ON DELETE CASCADE,
    region_id TEXT NOT NULL REFERENCES regions(id) ON DELETE RESTRICT,
    PRIMARY KEY (plan_id, region_id)
);

-- Preserve existing behaviour for every current subscription. Singapore is
-- deliberately not granted here; an administrator enables it on chosen plans.
INSERT INTO plan_regions (plan_id, region_id)
SELECT id, 'europe' FROM plans
ON CONFLICT DO NOTHING;

CREATE INDEX IF NOT EXISTS idx_plan_regions_region_id ON plan_regions(region_id);
