-- Replace day-based trial duration with minute-based, so sub-day trials (e.g. the
-- 2h credential-signup "taster") live on the plan itself instead of in app config.
-- Paid-period duration stays in days (period_days) — it's tied to Stars billing/invoices.
ALTER TABLE plans ADD COLUMN trial_minutes INT;

-- Backfill existing day-based trials (basic = 7 days -> 10080).
UPDATE plans SET trial_minutes = trial_days * 1440 WHERE trial_days IS NOT NULL;

-- The taster plan had no trial_days (duration lived in config: taster_trial_minutes=120).
UPDATE plans SET trial_minutes = 120 WHERE name = 'taster';

ALTER TABLE plans DROP COLUMN trial_days;
