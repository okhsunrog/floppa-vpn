-- Pin the closed value sets the bot writes (floppa_core::models::{NotificationKind,
-- LinkCodeKind, Lang}), the same way 0014 did for peers and subscriptions. NULL still passes:
-- telegram_link_codes.kind is stamped only on consumption and users.language only once the
-- client reported a supported language or the user chose one.

ALTER TABLE notification_log
    ADD CONSTRAINT notification_log_kind_check
    CHECK (kind IN ('expiry_1d_before', 'expiry_now'));

ALTER TABLE telegram_link_codes
    ADD CONSTRAINT telegram_link_codes_kind_check
    CHECK (kind IN ('simple', 'merge'));

ALTER TABLE users
    ADD CONSTRAINT users_language_check
    CHECK (language IN ('en', 'ru'));
