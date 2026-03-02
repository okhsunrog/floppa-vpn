-- Update plan speed limits
UPDATE plans SET default_speed_limit_mbps = 10 WHERE name = 'basic';
UPDATE plans SET default_speed_limit_mbps = 100 WHERE name = 'premium';

-- Remove friends plan (only if no subscriptions reference it)
DELETE FROM plans WHERE name = 'friends'
AND NOT EXISTS (SELECT 1 FROM subscriptions WHERE plan_id = plans.id);

-- Make expires_at nullable for permanent subscriptions
ALTER TABLE subscriptions ALTER COLUMN expires_at DROP NOT NULL;
