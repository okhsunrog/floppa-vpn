-- subscriptions.payment_id was never written or read: payments link to subscriptions the other
-- way round (payments.subscription_id) since the initial schema.
ALTER TABLE subscriptions DROP COLUMN IF EXISTS payment_id;
