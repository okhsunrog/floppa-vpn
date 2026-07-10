use crate::bot::i18n;
use chrono::{Duration, Utc};
use floppa_core::{Config, DbPool, Secrets, billing, services};
use teloxide::{
    dispatching::UpdateHandler,
    prelude::*,
    types::{
        InlineKeyboardButton, InlineKeyboardMarkup, KeyboardButton, KeyboardMarkup, LabeledPrice,
        ParseMode, PreCheckoutQuery, SuccessfulPayment, WebAppInfo,
    },
    utils::command::BotCommands,
};
use tracing::error;

type HandlerResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

#[derive(BotCommands, Clone)]
#[command(rename_rule = "lowercase", description = "Available commands:")]
pub enum Command {
    #[command(description = "Start the bot")]
    Start(String),
    #[command(description = "Check subscription status")]
    Status,
    #[command(description = "Purchase a subscription")]
    Buy,
    #[command(description = "Get VLESS config")]
    Vless,
    #[command(description = "Change language / Сменить язык")]
    Lang,
}

pub fn schema() -> UpdateHandler<Box<dyn std::error::Error + Send + Sync + 'static>> {
    use dptree::case;

    let command_handler = teloxide::filter_command::<Command, _>()
        .branch(case![Command::Start(payload)].endpoint(start))
        .branch(case![Command::Status].endpoint(status))
        .branch(case![Command::Buy].endpoint(buy))
        .branch(case![Command::Vless].endpoint(vless))
        .branch(case![Command::Lang].endpoint(lang));

    let callback_handler = Update::filter_callback_query().endpoint(handle_callback);

    // PreCheckoutQuery must be handled as a top-level update kind (not a message)
    let pre_checkout_handler = Update::filter_pre_checkout_query().endpoint(handle_pre_checkout);

    // Taps on the persistent reply keyboard arrive as plain text equal to a button label.
    let menu_button_handler =
        dptree::filter(|msg: Message| msg.text().and_then(i18n::match_menu_button).is_some())
            .endpoint(handle_menu_button);

    // SuccessfulPayment comes as a message — must be before commands/fallback
    let message_handler = Update::filter_message()
        .branch(Message::filter_successful_payment().endpoint(handle_successful_payment))
        .branch(command_handler)
        .branch(menu_button_handler)
        .endpoint(fallback);

    dptree::entry()
        .branch(pre_checkout_handler)
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

async fn start(
    bot: Bot,
    msg: Message,
    pool: DbPool,
    config: Config,
    payload: String,
) -> HandlerResult {
    // Deep-link account linking: /start link_<code>
    if let Some(code) = payload.strip_prefix("link_") {
        return start_with_link(bot, msg, pool, code.to_string()).await;
    }

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

    // Welcome message carries the persistent reply keyboard (quick actions).
    bot.send_message(msg.chat.id, text)
        .reply_markup(main_menu_keyboard(msgs))
        .await?;

    // Follow up with a prominent inline button that launches the Mini App, if configured.
    // (The chat menu button next to the input — set at startup — also opens it.)
    let web_app_url = config.bot.as_ref().and_then(|b| b.web_app_url.as_deref());
    if let Some(url) = web_app_url {
        let keyboard = InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::web_app(
            msgs.open_app,
            WebAppInfo { url: url.parse()? },
        )]]);
        bot.send_message(msg.chat.id, msgs.open_app_cta)
            .reply_markup(keyboard)
            .await?;
    }

    Ok(())
}

/// Persistent bottom reply keyboard with the most-used actions. Taps come back as plain
/// text equal to the button label and are routed in [`handle_menu_button`].
fn main_menu_keyboard(msgs: &i18n::Messages) -> KeyboardMarkup {
    KeyboardMarkup::new(vec![
        vec![
            KeyboardButton::new(msgs.btn_status),
            KeyboardButton::new(msgs.btn_buy),
        ],
        vec![KeyboardButton::new(msgs.btn_lang)],
    ])
    .resize_keyboard()
    .persistent()
}

/// Route a reply-keyboard tap to the matching command handler.
async fn handle_menu_button(bot: Bot, msg: Message, pool: DbPool, config: Config) -> HandlerResult {
    match msg.text().and_then(i18n::match_menu_button) {
        Some(i18n::BotMenuAction::Status) => status(bot, msg, pool).await,
        Some(i18n::BotMenuAction::Buy) => buy(bot, msg, pool, config).await,
        Some(i18n::BotMenuAction::Lang) => lang(bot, msg, pool).await,
        None => Ok(()),
    }
}

/// Mark a link code consumed (idempotent). Returns true if this call consumed it.
async fn consume_link_code(pool: &DbPool, code: &str, kind: &str) -> Result<bool, sqlx::Error> {
    let res = sqlx::query(
        "UPDATE telegram_link_codes SET consumed_at = NOW(), kind = $2 WHERE code = $1 AND consumed_at IS NULL AND expires_at > NOW()",
    )
    .bind(code)
    .bind(kind)
    .execute(pool)
    .await?;
    Ok(res.rows_affected() == 1)
}

/// Consume a link code and attach the Telegram identity in the same transaction. Returning `None`
/// means another request consumed the code first (or the session account is no longer attachable).
async fn attach_telegram_with_link_code(
    pool: &DbPool,
    code: &str,
    telegram_id: i64,
    username: Option<&str>,
    first_name: Option<&str>,
    last_name: Option<&str>,
) -> floppa_core::error::Result<Option<bool>> {
    let mut tx = pool.begin().await?;

    let session_user_id = sqlx::query_scalar::<_, i64>(
        r#"UPDATE telegram_link_codes
           SET consumed_at = NOW(), kind = 'simple'
           WHERE code = $1 AND consumed_at IS NULL AND expires_at > NOW()
           RETURNING user_id"#,
    )
    .bind(code)
    .fetch_optional(&mut *tx)
    .await?;

    let Some(session_user_id) = session_user_id else {
        tx.rollback().await?;
        return Ok(None);
    };

    let attached = sqlx::query(
        r#"UPDATE users SET
               telegram_id = $2,
               username = COALESCE(username, $3),
               first_name = COALESCE(first_name, $4),
               last_name = COALESCE(last_name, $5)
           WHERE id = $1 AND telegram_id IS NULL"#,
    )
    .bind(session_user_id)
    .bind(telegram_id)
    .bind(username)
    .bind(first_name)
    .bind(last_name)
    .execute(&mut *tx)
    .await?;

    if attached.rows_affected() != 1 {
        tx.rollback().await?;
        return Ok(None);
    }

    tx.commit().await?;

    // Trial claiming is independently idempotent; identity attachment and code consumption are the
    // security-sensitive pair that must commit together.
    let granted = services::grant_real_trial_if_unused(pool, session_user_id).await?;
    Ok(Some(granted))
}

/// Handle `/start link_<code>`: attach this Telegram to the session account, or (if the Telegram
/// already belongs to another account) offer a merge/recovery confirmation.
async fn start_with_link(bot: Bot, msg: Message, pool: DbPool, code: String) -> HandlerResult {
    let (telegram_id, msgs) = resolve_msg_lang(&msg, &pool).await;
    let username = msg.from.as_ref().and_then(|u| u.username.clone());
    let first_name = msg.from.as_ref().map(|u| u.first_name.clone());
    let last_name = msg.from.as_ref().and_then(|u| u.last_name.clone());

    // Resolve a valid, unconsumed, unexpired link code → the session account that minted it.
    let pending = sqlx::query!(
        "SELECT user_id FROM telegram_link_codes WHERE code = $1 AND consumed_at IS NULL AND expires_at > NOW()",
        code,
    )
    .fetch_optional(&pool)
    .await?;
    let Some(pending) = pending else {
        bot.send_message(msg.chat.id, msgs.link_invalid).await?;
        return Ok(());
    };
    let session_user_id = pending.user_id;

    // Does this Telegram already belong to an account?
    let existing = sqlx::query!("SELECT id FROM users WHERE telegram_id = $1", telegram_id)
        .fetch_optional(&pool)
        .await?;

    match existing {
        // Already this same account → nothing to do.
        Some(row) if row.id == session_user_id => {
            if consume_link_code(&pool, &code, "simple").await? {
                bot.send_message(msg.chat.id, msgs.link_already).await?;
            } else {
                bot.send_message(msg.chat.id, msgs.link_invalid).await?;
            }
        }
        // Belongs to a DIFFERENT established account → confirm merge/recovery.
        Some(husk) => {
            let info = sqlx::query!(
                r#"SELECT
                       created_at,
                       (SELECT COUNT(*) FROM app_installations WHERE user_id = $1) AS "devices!",
                       (SELECT p.display_name FROM subscriptions s
                          JOIN plans p ON s.plan_id = p.id
                          WHERE s.user_id = $1 AND (s.expires_at IS NULL OR s.expires_at > NOW())
                          LIMIT 1) AS plan
                   FROM users WHERE id = $1"#,
                husk.id,
            )
            .fetch_one(&pool)
            .await?;
            let text = i18n::format_link_merge_prompt(
                msgs,
                info.created_at,
                info.devices,
                info.plan.as_deref(),
            );
            let keyboard = InlineKeyboardMarkup::new(vec![
                vec![InlineKeyboardButton::callback(
                    msgs.link_merge_confirm,
                    format!("link_merge:{code}"),
                )],
                vec![InlineKeyboardButton::callback(
                    msgs.link_merge_cancel,
                    "link_cancel".to_string(),
                )],
            ]);
            bot.send_message(msg.chat.id, text)
                .reply_markup(keyboard)
                .await?;
        }
        // Fresh Telegram → simple attach + real trial.
        None => {
            let granted = attach_telegram_with_link_code(
                &pool,
                &code,
                telegram_id,
                username.as_deref(),
                first_name.as_deref(),
                last_name.as_deref(),
            )
            .await?;
            let Some(granted) = granted else {
                bot.send_message(msg.chat.id, msgs.link_invalid).await?;
                return Ok(());
            };
            let mut text = msgs.link_success.to_string();
            if granted {
                text.push_str("\n\n");
                text.push_str(msgs.trial_granted);
            }
            bot.send_message(msg.chat.id, text).await?;
        }
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

async fn buy(bot: Bot, msg: Message, pool: DbPool, config: Config) -> HandlerResult {
    let (_, msgs) = resolve_msg_lang(&msg, &pool).await;

    let plans = billing::get_purchasable_plans(&pool).await?;

    if plans.is_empty() {
        bot.send_message(msg.chat.id, msgs.buy_no_plans).await?;
        return Ok(());
    }

    let stars_rub_rate = config.bot.as_ref().and_then(|b| b.stars_rub_rate);

    let buttons: Vec<Vec<InlineKeyboardButton>> = plans
        .iter()
        .map(|p| {
            vec![InlineKeyboardButton::callback(
                i18n::format_plan_button(
                    msgs,
                    &p.display_name,
                    p.price_stars,
                    p.period_days,
                    stars_rub_rate,
                ),
                format!("buy:{}", p.id),
            )]
        })
        .collect();

    let keyboard = InlineKeyboardMarkup::new(buttons);
    let text = i18n::format_plans_message(msgs, msgs.buy_choose_plan, &plans);

    bot.send_message(msg.chat.id, text)
        .reply_markup(keyboard)
        .await?;

    Ok(())
}

async fn vless(
    bot: Bot,
    msg: Message,
    pool: DbPool,
    config: Config,
    secrets: Secrets,
) -> HandlerResult {
    let (telegram_id, msgs) = resolve_msg_lang(&msg, &pool).await;

    // Check VLESS is configured
    let reality_public_key = match secrets.vless.as_ref() {
        Some(v) if config.vless.is_some() => &v.reality_public_key,
        _ => {
            bot.send_message(msg.chat.id, msgs.vless_not_configured)
                .await?;
            return Ok(());
        }
    };

    // Look up user
    let user = sqlx::query!(
        "SELECT id, vless_uuid FROM users WHERE telegram_id = $1",
        telegram_id,
    )
    .fetch_optional(&pool)
    .await?;

    let user = match user {
        Some(u) => u,
        None => {
            bot.send_message(msg.chat.id, msgs.vless_no_user).await?;
            return Ok(());
        }
    };

    // Check active subscription
    let has_sub = sqlx::query_scalar!(
        r#"SELECT EXISTS(
            SELECT 1 FROM subscriptions
            WHERE user_id = $1 AND (expires_at IS NULL OR expires_at > NOW())
        ) as "exists!""#,
        user.id,
    )
    .fetch_one(&pool)
    .await?;

    if !has_sub {
        bot.send_message(msg.chat.id, msgs.no_subscription_short)
            .await?;
        return Ok(());
    }

    // Get or generate VLESS UUID
    let uuid = match user.vless_uuid {
        Some(uuid) => uuid,
        None => {
            let new_uuid = uuid::Uuid::new_v4().to_string();
            sqlx::query!(
                "UPDATE users SET vless_uuid = $1 WHERE id = $2",
                &new_uuid,
                user.id
            )
            .execute(&pool)
            .await?;
            new_uuid
        }
    };

    let uri = services::generate_vless_uri(&uuid, &config, reality_public_key)?;

    let text = format!("{}\n\n<code>{}</code>", msgs.vless_your_config, uri);

    bot.send_message(msg.chat.id, text)
        .parse_mode(ParseMode::Html)
        .await?;

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

        bot.answer_callback_query(q.id.clone()).await?;

        if let Some(msg) = q.message {
            bot.edit_message_text(msg.chat().id, msg.id(), msgs.lang_set)
                .await?;
        }
    } else if let Some(code) = data.strip_prefix("link_merge:") {
        let telegram_id = q.from.id.0 as i64;
        let msgs = i18n::resolve_lang(&pool, telegram_id, q.from.language_code.as_deref()).await;
        bot.answer_callback_query(q.id.clone()).await?;

        // Re-validate the code (it may have expired while the button waited) and re-resolve the husk.
        let pending = sqlx::query!(
            "SELECT user_id FROM telegram_link_codes WHERE code = $1 AND consumed_at IS NULL AND expires_at > NOW()",
            code,
        )
        .fetch_optional(&pool)
        .await?;
        let result_text = if let Some(pending) = pending {
            let husk = sqlx::query!("SELECT id FROM users WHERE telegram_id = $1", telegram_id)
                .fetch_optional(&pool)
                .await?;
            let merged = match husk {
                Some(h) if h.id != pending.user_id => {
                    services::merge_telegram_into_session(&pool, pending.user_id, h.id).await?
                }
                _ => false,
            };
            sqlx::query!(
                "UPDATE telegram_link_codes SET consumed_at = NOW(), kind = 'merge' WHERE code = $1 AND consumed_at IS NULL",
                code,
            )
            .execute(&pool)
            .await?;
            if merged {
                msgs.link_merge_done
            } else {
                msgs.link_invalid
            }
        } else {
            msgs.link_invalid
        };

        if let Some(msg) = q.message {
            bot.edit_message_text(msg.chat().id, msg.id(), result_text)
                .await?;
        }
    } else if data == "link_cancel" {
        let telegram_id = q.from.id.0 as i64;
        let msgs = i18n::resolve_lang(&pool, telegram_id, q.from.language_code.as_deref()).await;
        bot.answer_callback_query(q.id.clone()).await?;
        if let Some(msg) = q.message {
            bot.edit_message_text(msg.chat().id, msg.id(), msgs.link_cancelled)
                .await?;
        }
    } else if let Some(plan_id_str) = data.strip_prefix("buy:") {
        let plan_id: i32 = match plan_id_str.parse() {
            Ok(id) => id,
            Err(_) => return Ok(()),
        };

        let telegram_id = q.from.id.0 as i64;
        let msgs = i18n::resolve_lang(&pool, telegram_id, q.from.language_code.as_deref()).await;

        // Look up user
        let user = sqlx::query!("SELECT id FROM users WHERE telegram_id = $1", telegram_id,)
            .fetch_optional(&pool)
            .await?;
        let user_id = match user {
            Some(u) => u.id,
            None => return Ok(()),
        };

        // Look up the plan
        let plan = sqlx::query!(
            r#"
            SELECT id, display_name,
                   price_stars as "price_stars!", period_days as "period_days!"
            FROM plans
            WHERE id = $1 AND price_stars IS NOT NULL AND period_days IS NOT NULL AND is_public = true
            "#,
            plan_id,
        )
        .fetch_optional(&pool)
        .await?;
        let plan = match plan {
            Some(p) => p,
            None => return Ok(()),
        };

        // Calculate proration
        let current_sub = billing::get_current_subscription(&pool, user_id).await?;
        let proration =
            billing::calculate_proration(current_sub.as_ref(), plan.price_stars, plan.period_days);

        bot.answer_callback_query(q.id.clone()).await?;

        let chat_id = q
            .message
            .as_ref()
            .map(|m| m.chat().id)
            .unwrap_or(ChatId(telegram_id));

        if proration.payable_stars == 0 {
            // Credit covers the full price — switch with proportional days
            let result = billing::process_credit_switch(
                &pool,
                user_id,
                plan_id,
                proration.subscription_days,
                proration.credit_stars,
            )
            .await?;
            if result.is_some() {
                let expires = (Utc::now() + Duration::days(proration.subscription_days as i64))
                    .format("%Y-%m-%d")
                    .to_string();
                let message = i18n::format_buy_success(msgs, &plan.display_name, &expires);
                bot.send_message(chat_id, message).await?;
            }
            return Ok(());
        }

        // Send Stars invoice
        let payload = billing::build_invoice_payload(plan_id, user_id);
        let title = i18n::format_invoice_title(msgs, &plan.display_name, plan.period_days);
        let description = i18n::format_invoice_description(
            msgs,
            &plan.display_name,
            plan.period_days,
            proration.credit_stars,
        );

        bot.send_invoice(
            chat_id,
            title,
            description,
            payload,
            "XTR", // Telegram Stars currency
            vec![LabeledPrice::new(
                &plan.display_name,
                proration.payable_stars as u32,
            )],
        )
        .await?;
    }

    Ok(())
}

async fn handle_pre_checkout(bot: Bot, q: PreCheckoutQuery, pool: DbPool) -> HandlerResult {
    let (plan_id, payload_user_id) = match billing::parse_invoice_payload(&q.invoice_payload) {
        Some(ids) => ids,
        None => {
            bot.answer_pre_checkout_query(q.id.clone(), false)
                .error_message("Invalid invoice")
                .await?;
            return Ok(());
        }
    };

    // Verify plan exists and is purchasable
    let plan = sqlx::query!(
        r#"SELECT price_stars as "price_stars!", period_days as "period_days!" FROM plans WHERE id = $1 AND price_stars IS NOT NULL AND period_days IS NOT NULL"#,
        plan_id,
    )
    .fetch_optional(&pool)
    .await?;

    let plan = match plan {
        Some(p) => p,
        None => {
            bot.answer_pre_checkout_query(q.id.clone(), false)
                .error_message("Plan no longer available")
                .await?;
            return Ok(());
        }
    };

    // Verify user matches the one encoded in the payload
    let telegram_id = q.from.id.0 as i64;
    let user = sqlx::query!("SELECT id FROM users WHERE telegram_id = $1", telegram_id)
        .fetch_optional(&pool)
        .await?;

    let user = match user {
        Some(u) => u,
        None => {
            bot.answer_pre_checkout_query(q.id.clone(), false)
                .error_message("User not found. Please /start first.")
                .await?;
            return Ok(());
        }
    };

    if user.id != payload_user_id {
        bot.answer_pre_checkout_query(q.id.clone(), false)
            .error_message("User mismatch. Please try again.")
            .await?;
        return Ok(());
    }

    // Re-verify amount matches current proration
    let current_sub = billing::get_current_subscription(&pool, user.id).await?;
    let proration =
        billing::calculate_proration(current_sub.as_ref(), plan.price_stars, plan.period_days);

    if q.total_amount as i32 != proration.payable_stars {
        bot.answer_pre_checkout_query(q.id.clone(), false)
            .error_message("Price has changed. Please try again.")
            .await?;
        return Ok(());
    }

    bot.answer_pre_checkout_query(q.id.clone(), true).await?;

    Ok(())
}

async fn handle_successful_payment(
    bot: Bot,
    msg: Message,
    payment: SuccessfulPayment,
    pool: DbPool,
) -> HandlerResult {
    let (telegram_id, msgs) = resolve_msg_lang(&msg, &pool).await;

    let (plan_id, payload_user_id) = match billing::parse_invoice_payload(&payment.invoice_payload)
    {
        Some(ids) => ids,
        None => {
            error!(
                "Invalid invoice payload in successful payment: {}",
                payment.invoice_payload
            );
            bot.send_message(msg.chat.id, msgs.buy_error).await?;
            return Ok(());
        }
    };

    let user = sqlx::query!("SELECT id FROM users WHERE telegram_id = $1", telegram_id)
        .fetch_optional(&pool)
        .await?;
    let user_id = match user {
        Some(u) => u.id,
        None => {
            bot.send_message(msg.chat.id, msgs.buy_error).await?;
            return Ok(());
        }
    };

    if user_id != payload_user_id {
        error!("User mismatch in successful payment: expected {payload_user_id}, got {user_id}");
        bot.send_message(msg.chat.id, msgs.buy_error).await?;
        return Ok(());
    }

    let plan = sqlx::query!(
        "SELECT display_name, price_stars, period_days FROM plans WHERE id = $1",
        plan_id,
    )
    .fetch_optional(&pool)
    .await?;

    let plan = match plan {
        Some(p) => p,
        None => {
            bot.send_message(msg.chat.id, msgs.buy_error).await?;
            return Ok(());
        }
    };

    let period_days = plan.period_days.unwrap_or(30);
    let price_stars = plan.price_stars.unwrap_or(0);

    // Re-calculate proration for accurate credit recording
    let current_sub = billing::get_current_subscription(&pool, user_id).await?;
    let proration = billing::calculate_proration(current_sub.as_ref(), price_stars, period_days);

    match billing::complete_payment(
        &pool,
        billing::CompletePaymentParams {
            user_id,
            plan_id,
            period_days,
            telegram_charge_id: &payment.telegram_payment_charge_id.0,
            invoice_payload: &payment.invoice_payload,
            amount: payment.total_amount as i32,
            credit_amount: proration.credit_stars,
        },
    )
    .await
    {
        Ok(_) => {
            let expires = (Utc::now() + Duration::days(period_days as i64))
                .format("%Y-%m-%d")
                .to_string();
            let message = i18n::format_buy_success(msgs, &plan.display_name, &expires);
            bot.send_message(msg.chat.id, message).await?;
        }
        Err(e) => {
            // Idempotency: if telegram_charge_id UNIQUE violated, payment was already processed
            let is_duplicate = matches!(
                &e,
                floppa_core::error::FloppaError::Database(sqlx::Error::Database(pg_err))
                    if pg_err.constraint() == Some("payments_telegram_charge_id_key")
            );
            if is_duplicate {
                bot.send_message(msg.chat.id, msgs.buy_success).await?;
                return Ok(());
            }
            error!("Failed to complete payment: {e}");
            bot.send_message(msg.chat.id, msgs.buy_error).await?;
        }
    }

    Ok(())
}

async fn fallback(bot: Bot, msg: Message, pool: DbPool) -> HandlerResult {
    let (_, msgs) = resolve_msg_lang(&msg, &pool).await;
    bot.send_message(msg.chat.id, msgs.unknown_message).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Command, attach_telegram_with_link_code};
    use teloxide::utils::command::BotCommands;

    // Guards the deep-link split: a bare /start must still parse (new-user greeting path),
    // and /start link_<code> must capture the payload.
    #[test]
    fn start_parses_with_and_without_payload() {
        match Command::parse("/start", "floppabot") {
            Ok(Command::Start(s)) => assert!(s.is_empty(), "bare /start payload should be empty"),
            _ => panic!("bare /start did not parse to Start"),
        }
        match Command::parse("/start link_abc123", "floppabot") {
            Ok(Command::Start(s)) => assert_eq!(s, "link_abc123"),
            _ => panic!("/start link_ did not parse to Start"),
        }
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn telegram_link_code_has_exactly_one_winner(pool: floppa_core::DbPool) {
        let user_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO users (username) VALUES ('link-target') RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO telegram_link_codes (code, user_id, expires_at) \
             VALUES ('one-time-code', $1, NOW() + INTERVAL '10 minutes')",
        )
        .bind(user_id)
        .execute(&pool)
        .await
        .unwrap();

        let (first, second) = tokio::join!(
            attach_telegram_with_link_code(
                &pool,
                "one-time-code",
                11111,
                Some("first"),
                None,
                None,
            ),
            attach_telegram_with_link_code(
                &pool,
                "one-time-code",
                22222,
                Some("second"),
                None,
                None,
            ),
        );
        let winners = [first.unwrap(), second.unwrap()]
            .into_iter()
            .filter(Option::is_some)
            .count();
        assert_eq!(winners, 1);

        let telegram_id =
            sqlx::query_scalar::<_, Option<i64>>("SELECT telegram_id FROM users WHERE id = $1")
                .bind(user_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(matches!(telegram_id, Some(11111 | 22222)));
    }
}
