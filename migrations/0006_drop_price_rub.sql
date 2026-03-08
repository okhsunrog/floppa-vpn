-- Drop unused price_rub column from plans.
-- Payments use Telegram Stars (price_stars) only.
ALTER TABLE plans DROP COLUMN IF EXISTS price_rub;
