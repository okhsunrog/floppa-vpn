use crate::bot::i18n;
use floppa_core::{Config, DbPool, services};
use teloxide::{
    dispatching::UpdateHandler,
    prelude::*,
    types::{InlineKeyboardButton, InlineKeyboardMarkup, WebAppInfo},
    utils::command::BotCommands,
};

type HandlerResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

#[derive(BotCommands, Clone)]
#[command(rename_rule = "lowercase", description = "Available commands:")]
pub enum Command {
    #[command(description = "Start the bot")]
    Start,
    #[command(description = "Check subscription status")]
    Status,
    #[command(description = "Change language / Сменить язык")]
    Lang,
}

pub fn schema() -> UpdateHandler<Box<dyn std::error::Error + Send + Sync + 'static>> {
    use dptree::case;

    let command_handler = teloxide::filter_command::<Command, _>()
        .branch(case![Command::Start].endpoint(start))
        .branch(case![Command::Status].endpoint(status))
        .branch(case![Command::Lang].endpoint(lang));

    let callback_handler = Update::filter_callback_query().endpoint(handle_callback);

    let message_handler = Update::filter_message()
        .branch(command_handler)
        .endpoint(fallback);

    dptree::entry()
        .branch(message_handler)
        .branch(callback_handler)
}

/// Helper: extract telegram_id and language_code from a message, resolve i18n.
async fn resolve_msg_lang(msg: &Message, pool: &DbPool) -> (i64, &'static i18n::Messages) {
    let telegram_id = msg.from.as_ref().map(|u| u.id.0 as i64).unwrap_or(0);
    let telegram_lang = msg.from.as_ref().and_then(|u| u.language_code.as_deref());
    let msgs = i18n::resolve_lang(pool, telegram_id, telegram_lang).await;
    (telegram_id, msgs)
}

async fn start(bot: Bot, msg: Message, pool: DbPool, config: Config) -> HandlerResult {
    let (telegram_id, msgs) = resolve_msg_lang(&msg, &pool).await;
    let username = msg.from.as_ref().and_then(|u| u.username.clone());
    let first_name = msg.from.as_ref().map(|u| u.first_name.clone());
    let last_name = msg.from.as_ref().and_then(|u| u.last_name.clone());

    let result = services::upsert_user(
        &pool,
        telegram_id,
        username.as_deref(),
        services::TelegramProfile {
            first_name: first_name.as_deref(),
            last_name: last_name.as_deref(),
            photo_url: None, // Bot API doesn't provide photo_url in messages
        },
        false,
    )
    .await?;

    let mut text = msgs.welcome.to_string();
    if result.trial_granted {
        text.push_str("\n\n");
        text.push_str(msgs.trial_granted);
    }

    // Build inline keyboard with Mini App button if web_app_url is configured
    let web_app_url = config.bot.as_ref().and_then(|b| b.web_app_url.as_deref());
    if let Some(url) = web_app_url {
        let keyboard = InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::web_app(
            msgs.open_app,
            WebAppInfo { url: url.parse()? },
        )]]);
        bot.send_message(msg.chat.id, text)
            .reply_markup(keyboard)
            .await?;
    } else {
        bot.send_message(msg.chat.id, text).await?;
    }

    Ok(())
}

async fn status(bot: Bot, msg: Message, pool: DbPool) -> HandlerResult {
    let (telegram_id, msgs) = resolve_msg_lang(&msg, &pool).await;

    let sub = sqlx::query!(
        r#"
        SELECT p.display_name as plan, s.expires_at
        FROM subscriptions s
        JOIN plans p ON s.plan_id = p.id
        JOIN users u ON s.user_id = u.id
        WHERE u.telegram_id = $1
          AND (s.expires_at IS NULL OR s.expires_at > NOW())
        ORDER BY s.expires_at DESC NULLS FIRST
        LIMIT 1
        "#,
        telegram_id,
    )
    .fetch_optional(&pool)
    .await?;

    let message = match sub {
        Some(s) => {
            let expires_str = match s.expires_at {
                Some(dt) => dt.format("%Y-%m-%d").to_string(),
                None => msgs.permanent.to_string(),
            };
            i18n::format_status(msgs, &s.plan, &expires_str)
        }
        None => msgs.no_subscription_short.to_string(),
    };

    bot.send_message(msg.chat.id, message).await?;

    Ok(())
}

async fn lang(bot: Bot, msg: Message, pool: DbPool) -> HandlerResult {
    let (_, msgs) = resolve_msg_lang(&msg, &pool).await;

    let keyboard = InlineKeyboardMarkup::new(vec![vec![
        InlineKeyboardButton::callback("🇬🇧 English", "lang:en"),
        InlineKeyboardButton::callback("🇷🇺 Русский", "lang:ru"),
    ]]);

    bot.send_message(msg.chat.id, msgs.lang_prompt)
        .reply_markup(keyboard)
        .await?;

    Ok(())
}

async fn handle_callback(bot: Bot, q: CallbackQuery, pool: DbPool) -> HandlerResult {
    let data = match q.data.as_deref() {
        Some(d) => d,
        None => return Ok(()),
    };

    if let Some(lang) = data.strip_prefix("lang:") {
        let telegram_id = q.from.id.0 as i64;

        sqlx::query!(
            "UPDATE users SET language = $1 WHERE telegram_id = $2",
            lang,
            telegram_id
        )
        .execute(&pool)
        .await?;

        let msgs = i18n::for_lang(Some(lang));

        bot.answer_callback_query(&q.id).await?;

        if let Some(msg) = q.message {
            bot.edit_message_text(msg.chat().id, msg.id(), msgs.lang_set)
                .await?;
        }
    }

    Ok(())
}

async fn fallback(bot: Bot, msg: Message, pool: DbPool) -> HandlerResult {
    let (_, msgs) = resolve_msg_lang(&msg, &pool).await;
    bot.send_message(msg.chat.id, msgs.unknown_message).await?;
    Ok(())
}
