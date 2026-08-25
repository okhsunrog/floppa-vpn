-- Pin the closed value sets the bot writes (floppa_core::models::{NotificationKind,
-- LinkCodeKind, Lang}), the same way 0014 did for peers and subscriptions. NULL still passes:
-- telegram_link_codes.kind is stamped only on consumption and users.language only once the
-- client reported a supported language or the user chose one.
--
-- ADD CONSTRAINT validates existing rows, and this runs at daemon startup, so each nullable
-- column is normalised first: an outlier becomes NULL ("unknown") rather than failing the
-- deploy. users.language used to be copied straight out of the `lang:<…>` callback_data, which a
-- modified Telegram client controls, so a stray value there is possible in principle.
-- notification_log.kind is NOT NULL and has only ever been written from a SQL CASE over the two
-- literals below, so it is left untouched — a hand-edited row would surface as a failed migration
-- (see docs/DEPLOYMENT.md §5 for the pre-deploy checks).

ALTER TABLE notification_log
    ADD CONSTRAINT notification_log_kind_check
    CHECK (kind IN ('expiry_1d_before', 'expiry_now'));

UPDATE telegram_link_codes SET kind = NULL
    WHERE kind IS NOT NULL AND kind NOT IN ('simple', 'merge');
ALTER TABLE telegram_link_codes
    ADD CONSTRAINT telegram_link_codes_kind_check
    CHECK (kind IN ('simple', 'merge'));

UPDATE users SET language = NULL
    WHERE language IS NOT NULL AND language NOT IN ('en', 'ru');
ALTER TABLE users
    ADD CONSTRAINT users_language_check
    CHECK (language IN ('en', 'ru'));
