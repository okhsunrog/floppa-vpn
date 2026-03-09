-- Tracks bot notifications sent to users (expiry reminders, broadcasts, etc.)
-- Used to deduplicate: don't send the same notification type twice for the same subscription.
CREATE TABLE notification_log (
    id          BIGSERIAL PRIMARY KEY,
    user_id     BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- e.g. 'expiry_1d_before', 'expiry_now'
    kind        TEXT NOT NULL,
    -- Links to the subscription that triggered this notification (NULL for broadcasts)
    subscription_id BIGINT REFERENCES subscriptions(id) ON DELETE SET NULL,
    sent_at     TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Fast lookup: "did we already send this kind of notification for this subscription?"
CREATE UNIQUE INDEX uq_notification_sub_kind
    ON notification_log (subscription_id, kind)
    WHERE subscription_id IS NOT NULL;
