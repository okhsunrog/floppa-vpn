ALTER TABLE users ADD COLUMN IF NOT EXISTS trial_used_at TIMESTAMPTZ;

-- Backfill existing users who already had a trial/basic subscription
UPDATE users u SET trial_used_at = s.created_at
FROM subscriptions s
JOIN plans p ON s.plan_id = p.id
WHERE s.user_id = u.id AND p.name = 'basic'
AND u.trial_used_at IS NULL;
