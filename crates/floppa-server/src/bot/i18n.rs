/// Simple struct-based i18n for the Telegram bot.
/// Resolves language: DB preference → Telegram language_code → English.
use floppa_core::DbPool;

#[allow(dead_code)]
pub struct Messages {
    // /start
    pub welcome: &'static str,
    pub trial_granted: &'static str,
    pub open_app: &'static str,

    // /status
    pub status_plan: &'static str,
    pub status_expires: &'static str,
    pub no_subscription_short: &'static str,
    pub permanent: &'static str,

    // /lang
    pub lang_prompt: &'static str,
    pub lang_set: &'static str,

    // fallback
    pub unknown_message: &'static str,

    // errors
    pub error_generic: &'static str,
}

static EN: Messages = Messages {
    welcome: "Welcome to Floppa VPN!\n\n\
              Use the button below to manage your VPN configs and subscription.",

    trial_granted: "You've been granted a free 7-day Basic subscription!",
    open_app: "Open Floppa VPN",

    status_plan: "Plan",
    status_expires: "Expires",
    no_subscription_short: "No active subscription.\n\nContact admin to get started.",
    permanent: "Permanent",

    lang_prompt: "Choose your language:",
    lang_set: "Language set to English",

    unknown_message: "I only understand commands:\n\n\
                      /start — open the app\n\
                      /status — check subscription\n\
                      /lang — change language",

    error_generic: "An error occurred. Please try again later.",
};

static RU: Messages = Messages {
    welcome: "Добро пожаловать в Floppa VPN!\n\n\
              Используйте кнопку ниже для управления VPN-конфигами и подпиской.",

    trial_granted: "Вам предоставлена бесплатная 7-дневная подписка Basic!",
    open_app: "Открыть Floppa VPN",

    status_plan: "Тариф",
    status_expires: "Истекает",
    no_subscription_short: "Нет активной подписки.\n\nОбратитесь к администратору.",
    permanent: "Бессрочно",

    lang_prompt: "Выберите язык:",
    lang_set: "Язык изменён на русский",

    unknown_message: "Я понимаю только команды:\n\n\
                      /start — открыть приложение\n\
                      /status — проверить подписку\n\
                      /lang — сменить язык",

    error_generic: "Произошла ошибка. Попробуйте позже.",
};

/// Get messages for a language code string.
pub fn for_lang(lang_code: Option<&str>) -> &'static Messages {
    match lang_code {
        Some(code) if code.starts_with("ru") => &RU,
        _ => &EN,
    }
}

/// Resolve language for a user: DB preference → Telegram language_code → English.
pub async fn resolve_lang(
    pool: &DbPool,
    telegram_id: i64,
    telegram_lang: Option<&str>,
) -> &'static Messages {
    // Check DB preference first
    let db_lang = sqlx::query_scalar!(
        "SELECT language FROM users WHERE telegram_id = $1",
        telegram_id
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .flatten();

    if let Some(lang) = db_lang {
        return for_lang(Some(&lang));
    }

    // Fall back to Telegram language_code
    for_lang(telegram_lang)
}

/// Format status message with plan and expiry date.
pub fn format_status(msgs: &Messages, plan: &str, expires: &str) -> String {
    format!(
        "{}: {}\n{}: {}",
        msgs.status_plan, plan, msgs.status_expires, expires
    )
}
