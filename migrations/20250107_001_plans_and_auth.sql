-- Plans table for subscription tiers
CREATE TABLE plans (
    id SERIAL PRIMARY KEY,
    name TEXT UNIQUE NOT NULL,
    display_name TEXT NOT NULL,
    default_speed_limit_mbps INT,        -- NULL = unlimited
    default_traffic_limit_bytes BIGINT,  -- NULL = unlimited
    max_peers INT NOT NULL DEFAULT 1,
    price_cents INT NOT NULL DEFAULT 0,
    is_public BOOLEAN NOT NULL DEFAULT true,
    trial_days INT,                       -- NULL = not a trial plan
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Seed default plans
INSERT INTO plans (name, display_name, default_speed_limit_mbps, default_traffic_limit_bytes, max_peers, trial_days, is_public) VALUES
    ('trial', 'Free Trial', 5, NULL, 1, 7, true),
    ('standard', 'Standard', 50, NULL, 3, NULL, true),
    ('premium', 'Premium', NULL, NULL, 5, NULL, true),
    ('friends', 'Friends & Family', NULL, NULL, 10, NULL, false);

-- Add admin flag to users
ALTER TABLE users ADD COLUMN is_admin BOOLEAN NOT NULL DEFAULT false;

-- Replace plan string with plan_id reference in subscriptions
-- First add the new column (nullable for transition)
ALTER TABLE subscriptions ADD COLUMN plan_id INT REFERENCES plans(id);

-- Migrate existing data (if any) - match by name
UPDATE subscriptions s SET plan_id = p.id FROM plans p WHERE s.plan = p.name;

-- For any unmatched, default to standard
UPDATE subscriptions SET plan_id = (SELECT id FROM plans WHERE name = 'standard') WHERE plan_id IS NULL;

-- Now make it NOT NULL and drop old column
ALTER TABLE subscriptions ALTER COLUMN plan_id SET NOT NULL;
ALTER TABLE subscriptions DROP COLUMN plan;

-- Index for plan lookups
CREATE INDEX idx_subscriptions_plan_id ON subscriptions(plan_id);
CREATE INDEX idx_plans_is_public ON plans(is_public);
