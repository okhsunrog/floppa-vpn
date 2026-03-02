-- Add composite index for frequent subscription lookups by user_id + expires_at
CREATE INDEX IF NOT EXISTS idx_subscriptions_user_expires
    ON subscriptions (user_id, expires_at DESC NULLS FIRST);
