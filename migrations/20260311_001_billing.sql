-- Telegram Stars billing: add pricing/period to plans, create payments table,
-- and track subscription source (trial, purchase, admin_grant).

-- Add Stars pricing and billing period to plans
ALTER TABLE plans ADD COLUMN price_stars INT;    -- NULL = not purchasable with Stars
ALTER TABLE plans ADD COLUMN period_days INT;    -- NULL = not periodic (admin-only permanent)

-- Set billing period on existing purchasable plans
UPDATE plans SET period_days = 30 WHERE name = 'standard';
UPDATE plans SET period_days = 30 WHERE name = 'premium';
-- price_stars intentionally left NULL — set via admin panel when ready to enable /buy

-- Payments table (multi-rail ready: provider + currency support future payment methods)
CREATE TABLE payments (
    id BIGSERIAL PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES users(id),
    plan_id INT NOT NULL REFERENCES plans(id),
    provider TEXT NOT NULL DEFAULT 'telegram_stars',
    currency TEXT NOT NULL DEFAULT 'XTR',
    amount INT NOT NULL,                   -- Stars actually charged
    credit_amount INT NOT NULL DEFAULT 0,  -- proration credit applied
    status TEXT NOT NULL DEFAULT 'pending', -- pending, completed, failed
    telegram_charge_id TEXT UNIQUE,        -- idempotency key from Telegram
    invoice_payload TEXT NOT NULL,          -- our internal payload
    subscription_id BIGINT REFERENCES subscriptions(id),
    provider_data JSONB,                   -- raw payment data for audit
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ
);

CREATE INDEX idx_payments_user_id ON payments(user_id);
CREATE INDEX idx_payments_status ON payments(status);

-- Track how each subscription was created: trial, purchase, or admin_grant.
ALTER TABLE subscriptions ADD COLUMN source TEXT NOT NULL DEFAULT 'admin_grant';
