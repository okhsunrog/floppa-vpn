-- A user has at most ONE current subscription.
--
-- Semantics:
--   * `subscriptions.is_current` marks the single row that IS the user's subscription right now.
--     Every other row of that user is history. The partial unique index below makes a second
--     current row impossible, whatever the writer.
--   * "Active" = current AND not expired: `is_current AND (expires_at IS NULL OR expires_at > NOW())`.
--     A current row past its expiry means "this user's subscription has run out" (the daemon
--     removes their peers); a user with no current row at all has never had one.
--   * The only writer of `is_current` is `floppa_core::services::replace_active_subscription`
--     (trials, purchases, admin grants) plus the Telegram-merge path, which keeps the later-
--     expiring side. Both demote the previous current row and close it out (`expires_at = NOW()`
--     if it was still running), so history shows when each subscription ended.
--   * Readers never touch `subscriptions` for "the user's plan" directly: the view
--     `current_subscriptions` (subscription + plan columns, `is_active` precomputed) is the one
--     definition, so the daemon, the API, the bot and the VLESS registry cannot disagree.

ALTER TABLE subscriptions ADD COLUMN is_current BOOLEAN NOT NULL DEFAULT false;

-- Backfill: per user, the active row with the greatest expiry (permanent = NULL counts as
-- greatest); if none is active, the most recently created row, so the user's history still has
-- a "latest" and `is_active` on it reads false.
WITH ranked AS (
    SELECT id,
           ROW_NUMBER() OVER (
               PARTITION BY user_id
               ORDER BY (expires_at IS NULL OR expires_at > NOW()) DESC,
                        expires_at DESC NULLS FIRST,
                        created_at DESC,
                        id DESC
           ) AS rn
    FROM subscriptions
)
UPDATE subscriptions s SET is_current = true
FROM ranked r
WHERE r.id = s.id AND r.rn = 1;

CREATE UNIQUE INDEX subscriptions_one_current ON subscriptions(user_id) WHERE is_current;

-- The per-user "latest expiry" index served the LIMIT 1 lookups this migration retires.
DROP INDEX IF EXISTS idx_subscriptions_user_expires;

CREATE VIEW current_subscriptions AS
SELECT s.id,
       s.user_id,
       s.plan_id,
       s.starts_at,
       s.expires_at,
       s.source,
       s.created_at,
       (s.expires_at IS NULL OR s.expires_at > NOW()) AS is_active,
       p.name                        AS plan_name,
       p.display_name                AS plan_display_name,
       p.default_speed_limit_mbps    AS speed_limit_mbps,
       p.max_peers,
       p.price_stars,
       p.period_days,
       p.trial_minutes
FROM subscriptions s
JOIN plans p ON p.id = s.plan_id
WHERE s.is_current;

-- Demoting a row (merge) changes what the daemon must enforce even when plan/expiry stay put.
CREATE OR REPLACE FUNCTION notify_subscription_changed()
RETURNS TRIGGER AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        PERFORM pg_notify('subscription_changed', OLD.user_id::text);
        RETURN OLD;
    END IF;
    IF TG_OP = 'INSERT' OR
       (TG_OP = 'UPDATE' AND (
           OLD.plan_id IS DISTINCT FROM NEW.plan_id OR
           OLD.expires_at IS DISTINCT FROM NEW.expires_at OR
           OLD.is_current IS DISTINCT FROM NEW.is_current
       ))
    THEN
        PERFORM pg_notify('subscription_changed', NEW.user_id::text);
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

-- A plan's speed-limit change only matters to users it is CURRENTLY serving.
CREATE OR REPLACE FUNCTION notify_plan_changed() RETURNS TRIGGER AS $$
DECLARE
    affected_user_id BIGINT;
BEGIN
    IF TG_OP = 'UPDATE' AND
       OLD.default_speed_limit_mbps IS NOT DISTINCT FROM NEW.default_speed_limit_mbps
    THEN
        RETURN NEW;
    END IF;

    FOR affected_user_id IN
        SELECT cs.user_id FROM current_subscriptions cs
        WHERE cs.plan_id = NEW.id AND cs.is_active
    LOOP
        PERFORM pg_notify('subscription_changed', affected_user_id::text);
    END LOOP;

    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
