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

    // /buy
    pub buy_choose_plan: &'static str,
    pub buy_no_plans: &'static str,
    pub buy_success: &'static str,
    pub buy_error: &'static str,
    pub buy_plan_days: &'static str, // "days" / "дней"

    // /lang
    pub lang_prompt: &'static str,
    pub lang_set: &'static str,

    // /vless
    pub vless_your_config: &'static str,
    pub vless_open: &'static str,
    pub vless_not_configured: &'static str,
    pub vless_no_user: &'static str,

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
    no_subscription_short: "No active subscription.\n\nUse /buy to purchase a plan.",
    permanent: "Permanent",

    buy_choose_plan: "Choose a plan to purchase:",
    buy_no_plans: "No plans available for purchase at this time.",
    buy_success: "Payment successful! Your subscription has been activated.",
    buy_error: "Payment processing failed. Please try again or contact support.",
    buy_plan_days: "days",

    lang_prompt: "Choose your language:",
    lang_set: "Language set to English",

    vless_your_config: "Your VLESS config:",
    vless_open: "Open in app",
    vless_not_configured: "VLESS is not configured on this server.",
    vless_no_user: "Please use /start first.",

    unknown_message: "I only understand commands:\n\n\
                      /start — open the app\n\
                      /status — check subscription\n\
                      /buy — purchase a plan\n\
                      /vless — get VLESS config\n\
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
    no_subscription_short: "Нет активной подписки.\n\nИспользуйте /buy для покупки тарифа.",
    permanent: "Бессрочно",

    buy_choose_plan: "Выберите тариф для покупки:",
    buy_no_plans: "Сейчас нет тарифов, доступных для покупки.",
    buy_success: "Оплата прошла успешно! Подписка активирована.",
    buy_error: "Ошибка обработки платежа. Попробуйте снова или обратитесь в поддержку.",
    buy_plan_days: "дней",

    lang_prompt: "Выберите язык:",
    lang_set: "Язык изменён на русский",

    vless_your_config: "Ваш VLESS конфиг:",
    vless_open: "Открыть в приложении",
    vless_not_configured: "VLESS не настроен на этом сервере.",
    vless_no_user: "Сначала используйте /start.",

    unknown_message: "Я понимаю только команды:\n\n\
                      /start — открыть приложение\n\
                      /status — проверить подписку\n\
                      /buy — купить тариф\n\
                      /vless — получить VLESS конфиг\n\
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

/// Format plan button text: "Premium — 250 ⭐ (~450 ₽) / 30 days"
pub fn format_plan_button(
    msgs: &Messages,
    name: &str,
    stars: i32,
    days: i32,
    stars_rub_rate: Option<f64>,
) -> String {
    match stars_rub_rate {
        Some(rate) => {
            let rub = (stars as f64 * rate).round() as i64;
            format!(
                "{name} — {stars} ⭐ (~{rub} ₽) / {days} {}",
                msgs.buy_plan_days
            )
        }
        None => format!("{name} — {stars} ⭐ / {days} {}", msgs.buy_plan_days),
    }
}

/// Format invoice title: "Premium (30 days)"
pub fn format_invoice_title(msgs: &Messages, name: &str, days: i32) -> String {
    format!("{name} ({days} {})", msgs.buy_plan_days)
}

/// Format invoice description with optional proration info.
pub fn format_invoice_description(msgs: &Messages, name: &str, days: i32, credit: i32) -> String {
    if credit > 0 {
        format!(
            "{name} — {days} {}\n(-{credit} ⭐ credit)",
            msgs.buy_plan_days
        )
    } else {
        format!("{name} — {days} {}", msgs.buy_plan_days)
    }
}

/// Format success message with plan name and expiry date.
pub fn format_buy_success(msgs: &Messages, plan: &str, expires: &str) -> String {
    format!(
        "{}\n\n{}: {}\n{}: {}",
        msgs.buy_success, msgs.status_plan, plan, msgs.status_expires, expires
    )
}
