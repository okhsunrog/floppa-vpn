-- Rename price_cents to price_rub (integer rubles, Russia-first pricing)
ALTER TABLE plans RENAME COLUMN price_cents TO price_rub;
