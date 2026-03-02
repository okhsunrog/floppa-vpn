-- User language preference for bot i18n (NULL = auto-detect from Telegram)
ALTER TABLE users ADD COLUMN language TEXT;
