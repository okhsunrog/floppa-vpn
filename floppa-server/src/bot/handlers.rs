use crate::bot::callback::CallbackAction;
use crate::bot::i18n;
use chrono::Utc;
use dptree::di::DependencyMap;
use floppa_core::models::Lang;
use floppa_core::{Config, DbPool, Secrets, billing, services};
use std::ops::ControlFlow;
use std::sync::Arc;
use teloxide::{
    dispatching::UpdateHandler,
    prelude::*,
    types::{
        InlineKeyboardButton, InlineKeyboardMarkup, KeyboardButton, KeyboardMarkup, LabeledPrice,
        ParseMode, PreCheckoutQuery, SuccessfulPayment, UpdateKind, User, WebAppInfo,
    },
    utils::command::BotCommands,
};
use tracing::{error, warn};

/// What a bot handler can fail with. Every variant is unexpected from the user's point of view
/// (the expected outcomes — invalid link, no subscription, … — are replies, not errors), so
/// [`report_errors`] answers all of them with the same generic apology before they are logged.
#[derive(Debug, thiserror::Error)]
pub enum BotError {
    #[error(transparent)]
    Core(#[from] floppa_core::FloppaError),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error("Telegram API request failed: {0}")]
    Telegram(#[from] teloxide::RequestError),
}

type HandlerResult = Result<(), BotError>;

/// The commands the bot parses. Their menu descriptions live in [`i18n::bot_commands`], per
/// language; the test below keeps that table and this enum in step.
#[derive(BotCommands, Clone)]
#[command(rename_rule = "lowercase")]
pub enum Command {
    Start(String),
    Status,
    Buy,
    Vless,
    Lang,
}

pub fn schema() -> UpdateHandler<BotError> {
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

    // The bot is a personal assistant: messages and button taps coming from groups or channels
    // are dropped, so `/vless` can never publish someone's private URI to a whole chat and the
    // fallback never spams a group. Pre-checkout queries carry no chat and are always personal.
    let private_chats_only = dptree::filter(is_private_chat)
        .branch(message_handler)
        .branch(callback_handler);

    report_errors(
        dptree::entry()
            .branch(pre_checkout_handler)
            .branch(private_chats_only),
    )
}

/// Wrap a handler tree so that a failed endpoint still answers the user.
///
/// The dispatcher's error handler only ever sees the error value, not the update it came from,
/// so a failure would otherwise vanish into a log line: the user gets no reply, and a failed
/// pre-checkout is left unanswered until Telegram gives up on it. This layer runs the inner
/// tree, and on `Err` replies with `error_generic` in the user's language — declining the
/// pre-checkout with that text when the update is one — before handing the error on for
/// logging. The inner tree's signature and update-kind description are kept, so dptree's
/// type checking and the dispatcher's `allowed_updates` still see the real tree.
fn report_errors(inner: UpdateHandler<BotError>) -> UpdateHandler<BotError> {
    let signature = inner.sig().clone();
    let description = inner.description().clone();
    dptree::from_fn_with_description(
        description,
        move |deps: DependencyMap, cont| {
            let inner = inner.clone();
            async move {
                let flow = inner.execute(deps.clone(), cont).await;
                if let ControlFlow::Break(Err(err)) = &flow {
                    let bot: Arc<Bot> = deps.get();
                    let update: Arc<Update> = deps.get();
                    let pool: Arc<DbPool> = deps.get();
                    tell_user_it_failed(&bot, &update, &pool, err).await;
                }
                flow
            }
        },
        signature,
    )
}

/// Best-effort "something went wrong" reply for the update a handler failed on.
async fn tell_user_it_failed(bot: &Bot, update: &Update, pool: &DbPool, err: &BotError) {
    let msgs = match update.from() {
        Some(user) => {
            i18n::resolve_lang(pool, user.id.0 as i64, user.language_code.as_deref()).await
        }
        None => i18n::for_language_tag(None),
    };
    let sent = match &update.kind {
        // Not answering a pre-checkout leaves the payment hanging; declining it keeps the
        // user's Stars untouched and shows them the reason.
        UpdateKind::PreCheckoutQuery(q) => bot
            .answer_pre_checkout_query(q.id.clone(), false)
            .error_message(msgs.error_generic)
            .await
            .map(drop),
        _ => match update.chat() {
            Some(chat) => bot
                .send_message(chat.id, msgs.error_generic)
                .await
                .map(drop),
            None => Ok(()),
        },
    };
    if let Err(reply_err) = sent {
        warn!("Could not report a handler failure ({err}) to the user: {reply_err}");
    }
}

/// True for updates that originate in a one-to-one chat with the bot. For a callback query this
/// is the chat of the message the inline keyboard is attached to.
fn is_private_chat(update: Update) -> bool {
    update.chat().is_some_and(|chat| chat.is_private())
}

/// Who an update is from, resolved once at the top of a handler: the Telegram user, the
/// account they have with us (if they ever `/start`ed) and the messages in their language.
struct Caller {
    telegram_id: i64,
    user: Option<services::BotUser>,
    msgs: &'static i18n::Messages,
}

impl Caller {
    async fn resolve(pool: &DbPool, from: &User) -> Result<Self, BotError> {
        let telegram_id = from.id.0 as i64;
        let user = services::find_bot_user(pool, telegram_id).await?;
        let msgs = i18n::for_user(user.as_ref(), from.language_code.as_deref());
        Ok(Caller {
            telegram_id,
            user,
            msgs,
        })
    }
}

/// The sender of a message. Absent only for channel posts and anonymous group admins, which the
/// private-chat filter already excludes — so `None` means there is nobody to answer.
fn sender(msg: &Message) -> Option<&User> {
    msg.from.as_ref()
}

/// Welcome text, plus the trial announcement when one was just granted.
fn welcome_text(msgs: &i18n::Messages, base: &str, trial: Option<&services::TrialGrant>) -> String {
    let mut text = base.to_string();
    if let Some(trial) = trial {
        text.push_str("\n\n");
        text.push_str(&i18n::format_trial_granted(msgs, trial));
    }
    text
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

    let Some(from) = sender(&msg) else {
        return Ok(());
    };
    let caller = Caller::resolve(&pool, from).await?;
    let msgs = caller.msgs;

    let result = services::upsert_user(
        &pool,
        caller.telegram_id,
        from.username.as_deref(),
        services::TelegramProfile {
            first_name: Some(&from.first_name),
            last_name: from.last_name.as_deref(),
            photo_url: None, // Bot API doesn't provide photo_url in messages
            language: from
                .language_code
                .as_deref()
                .and_then(Lang::from_language_tag),
        },
        false,
    )
    .await?;

    let text = welcome_text(msgs, msgs.welcome, result.trial.as_ref());

    // Welcome message carries the persistent reply keyboard (quick actions).
    bot.send_message(msg.chat.id, text)
        .reply_markup(main_menu_keyboard(msgs))
        .await?;

    // Follow up with a prominent inline button that launches the Mini App, if configured.
    // (The chat menu button next to the input — set at startup — also opens it.)
    let web_app_url = config.bot.as_ref().and_then(|b| b.web_app_url.clone());
    if let Some(url) = web_app_url {
        let keyboard = InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::web_app(
            msgs.open_app,
            WebAppInfo { url },
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

/// The Telegram identity behind a message or callback, as core wants it for linking.
fn telegram_identity(user: &User) -> services::TelegramIdentity<'_> {
    services::TelegramIdentity {
        telegram_id: user.id.0 as i64,
        username: user.username.as_deref(),
        first_name: Some(user.first_name.as_str()),
        last_name: user.last_name.as_deref(),
        language: user
            .language_code
            .as_deref()
            .and_then(Lang::from_language_tag),
    }
}

/// Handle `/start link_<code>`: attach this Telegram to the session account, or (if the Telegram
/// already belongs to another account) offer a merge/recovery confirmation.
async fn start_with_link(bot: Bot, msg: Message, pool: DbPool, code: String) -> HandlerResult {
    let Some(from) = sender(&msg) else {
        return Ok(());
    };
    let msgs = Caller::resolve(&pool, from).await?.msgs;

    match services::begin_telegram_link(&pool, &code, telegram_identity(from)).await? {
        services::LinkStart::Attached { trial } => {
            let text = welcome_text(msgs, msgs.link_success, trial.as_ref());
            bot.send_message(msg.chat.id, text).await?;
        }
        services::LinkStart::AlreadyLinked => {
            bot.send_message(msg.chat.id, msgs.link_already).await?;
        }
        services::LinkStart::InvalidCode => {
            bot.send_message(msg.chat.id, msgs.link_invalid).await?;
        }
        services::LinkStart::MergeRequired(prompt) => {
            let keyboard = InlineKeyboardMarkup::new(vec![
                vec![InlineKeyboardButton::callback(
                    msgs.link_merge_confirm,
                    CallbackAction::LinkMerge {
                        code: prompt.code.clone(),
                    }
                    .to_string(),
                )],
                vec![InlineKeyboardButton::callback(
                    msgs.link_merge_cancel,
                    CallbackAction::LinkCancel.to_string(),
                )],
            ]);
            bot.send_message(msg.chat.id, i18n::format_link_merge_prompt(msgs, &prompt))
                .reply_markup(keyboard)
                .await?;
        }
    }

    Ok(())
}

async fn status(bot: Bot, msg: Message, pool: DbPool) -> HandlerResult {
    let Some(from) = sender(&msg) else {
        return Ok(());
    };
    let caller = Caller::resolve(&pool, from).await?;
    let msgs = caller.msgs;

    let sub = match caller.user {
        Some(user) => billing::get_current_subscription(&pool, user.id).await?,
        None => None,
    };

    let message = match sub {
        Some(s) => {
            let expires_str = match s.expires_at {
                Some(dt) => format_expiry(dt),
                None => msgs.permanent.to_string(),
            };
            i18n::format_status(msgs, &s.plan_display_name, &expires_str)
        }
        None => msgs.no_subscription_short.to_string(),
    };

    bot.send_message(msg.chat.id, message).await?;

    Ok(())
}

async fn buy(bot: Bot, msg: Message, pool: DbPool, config: Config) -> HandlerResult {
    let Some(from) = sender(&msg) else {
        return Ok(());
    };
    let caller = Caller::resolve(&pool, from).await?;
    let msgs = caller.msgs;

    // A permanent subscription has nothing to upgrade to; buying would only replace "forever"
    // with a paid month (billing refuses that too, but the user deserves a clear answer).
    if let Some(user) = caller.user
        && holds_permanent_subscription(&pool, user.id).await?
    {
        bot.send_message(msg.chat.id, msgs.buy_permanent).await?;
        return Ok(());
    }

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
                CallbackAction::Buy { plan_id: p.id }.to_string(),
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

/// Whether the user's current subscription is permanent (`expires_at IS NULL`).
async fn holds_permanent_subscription(pool: &DbPool, user_id: i64) -> Result<bool, BotError> {
    let current = billing::get_current_subscription(pool, user_id).await?;
    Ok(current.is_some_and(|sub| sub.expires_at.is_none()))
}

async fn vless(
    bot: Bot,
    msg: Message,
    pool: DbPool,
    config: Config,
    secrets: Secrets,
) -> HandlerResult {
    let Some(from) = sender(&msg) else {
        return Ok(());
    };
    let caller = Caller::resolve(&pool, from).await?;
    let msgs = caller.msgs;

    // Check VLESS is configured
    let reality_public_key = match secrets.vless.as_ref() {
        Some(v) if config.vless.is_some() => &v.reality_public_key,
        _ => {
            bot.send_message(msg.chat.id, msgs.vless_not_configured)
                .await?;
            return Ok(());
        }
    };

    let Some(user) = caller.user else {
        bot.send_message(msg.chat.id, msgs.vless_no_user).await?;
        return Ok(());
    };
    let user_id = user.id;

    // Check active subscription
    let has_sub = sqlx::query_scalar!(
        r#"SELECT EXISTS(
            SELECT 1 FROM subscriptions
            WHERE user_id = $1 AND (expires_at IS NULL OR expires_at > NOW())
        ) as "exists!""#,
        user_id,
    )
    .fetch_one(&pool)
    .await?;

    if !has_sub {
        bot.send_message(msg.chat.id, msgs.no_subscription_short)
            .await?;
        return Ok(());
    }

    // Same get-or-create as the app's /me/vless-config, so both agree on one UUID.
    let uuid = services::ensure_vless_uuid(&pool, user_id).await?;
    let uri = services::generate_vless_uri(&uuid.to_string(), &config, reality_public_key)?;

    let text = format!("{}\n\n<code>{}</code>", msgs.vless_your_config, uri);

    bot.send_message(msg.chat.id, text)
        .parse_mode(ParseMode::Html)
        .await?;

    Ok(())
}

async fn lang(bot: Bot, msg: Message, pool: DbPool) -> HandlerResult {
    let Some(from) = sender(&msg) else {
        return Ok(());
    };
    let msgs = Caller::resolve(&pool, from).await?.msgs;

    let keyboard = InlineKeyboardMarkup::new(vec![vec![
        InlineKeyboardButton::callback("🇬🇧 English", CallbackAction::SetLang(Lang::En).to_string()),
        InlineKeyboardButton::callback("🇷🇺 Русский", CallbackAction::SetLang(Lang::Ru).to_string()),
    ]]);

    bot.send_message(msg.chat.id, msgs.lang_prompt)
        .reply_markup(keyboard)
        .await?;

    Ok(())
}

async fn handle_callback(bot: Bot, q: CallbackQuery, pool: DbPool) -> HandlerResult {
    // Acknowledge first: the client shows a spinner on the tapped button until the query is
    // answered, and every early return below would otherwise leave it spinning.
    bot.answer_callback_query(q.id.clone()).await?;

    let action = match q.data.as_deref().map(str::parse::<CallbackAction>) {
        Some(Ok(action)) => action,
        // A button from an older bot version, or made-up data: nothing to do.
        Some(Err(e)) => {
            warn!("Ignoring callback from telegram_id={}: {e}", q.from.id);
            return Ok(());
        }
        None => return Ok(()),
    };

    let caller = Caller::resolve(&pool, &q.from).await?;
    let msgs = caller.msgs;

    if let CallbackAction::SetLang(lang) = action {
        sqlx::query!(
            "UPDATE users SET language = $1 WHERE telegram_id = $2",
            lang as _,
            caller.telegram_id
        )
        .execute(&pool)
        .await?;

        let msgs = i18n::for_lang(lang);

        if let Some(msg) = q.message {
            bot.edit_message_text(msg.chat().id, msg.id(), msgs.lang_set)
                .await?;
        }
    } else if let CallbackAction::LinkMerge { code } = &action {
        // The code may have expired or been spent while the button waited; core re-resolves
        // both accounts and spends the code atomically with whatever it ends up doing.
        let outcome =
            services::confirm_telegram_merge(&pool, code, telegram_identity(&q.from)).await?;
        let result_text = match outcome {
            services::MergeOutcome::Merged { .. } => msgs.link_merge_done.to_string(),
            services::MergeOutcome::AlreadyLinked => msgs.link_already.to_string(),
            services::MergeOutcome::Attached { trial } => {
                welcome_text(msgs, msgs.link_success, trial.as_ref())
            }
            services::MergeOutcome::InvalidCode => msgs.link_invalid.to_string(),
        };

        if let Some(msg) = q.message {
            bot.edit_message_text(msg.chat().id, msg.id(), result_text)
                .await?;
        }
    } else if action == CallbackAction::LinkCancel {
        if let Some(msg) = q.message {
            bot.edit_message_text(msg.chat().id, msg.id(), msgs.link_cancelled)
                .await?;
        }
    } else if let CallbackAction::Buy { plan_id } = action {
        let Some(user) = caller.user else {
            return Ok(());
        };
        let user_id = user.id;

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

        let chat_id = q
            .message
            .as_ref()
            .map(|m| m.chat().id)
            .unwrap_or(ChatId(caller.telegram_id));

        // Calculate proration
        let current_sub = billing::get_current_subscription(&pool, user_id).await?;
        if current_sub
            .as_ref()
            .is_some_and(|sub| sub.expires_at.is_none())
        {
            bot.send_message(chat_id, msgs.buy_permanent).await?;
            return Ok(());
        }
        let proration = billing::calculate_proration(
            current_sub.as_ref(),
            plan.price_stars,
            plan.period_days,
            Utc::now(),
        );

        if proration.payable_stars == 0 {
            // Credit covers the full price — switch with proportional days
            let switched = billing::process_credit_switch(
                &pool,
                user_id,
                plan_id,
                proration.subscription_days,
                proration.credit_stars,
            )
            .await;
            match switched {
                Ok(Some(outcome)) => {
                    let message = i18n::format_buy_success(
                        msgs,
                        &plan.display_name,
                        &format_expiry(outcome.expires_at),
                    );
                    bot.send_message(chat_id, message).await?;
                }
                // Duplicate tap within the dedup window: the first one already answered.
                Ok(None) => {}
                Err(billing::PurchaseError::PermanentSubscription { .. }) => {
                    bot.send_message(chat_id, msgs.buy_permanent).await?;
                }
                Err(billing::PurchaseError::AlreadyProcessed { .. }) => {
                    bot.send_message(chat_id, msgs.buy_success).await?;
                }
                Err(billing::PurchaseError::Core(e)) => return Err(e.into()),
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
    let caller = Caller::resolve(&pool, &q.from).await?;

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
    let Some(user) = caller.user else {
        bot.answer_pre_checkout_query(q.id.clone(), false)
            .error_message("User not found. Please /start first.")
            .await?;
        return Ok(());
    };

    if user.id != payload_user_id {
        bot.answer_pre_checkout_query(q.id.clone(), false)
            .error_message("User mismatch. Please try again.")
            .await?;
        return Ok(());
    }

    // Re-verify amount matches current proration
    let current_sub = billing::get_current_subscription(&pool, user.id).await?;
    if current_sub
        .as_ref()
        .is_some_and(|sub| sub.expires_at.is_none())
    {
        // Granted permanent access since the invoice was sent: do not take the money.
        bot.answer_pre_checkout_query(q.id.clone(), false)
            .error_message(caller.msgs.buy_permanent)
            .await?;
        return Ok(());
    }
    let proration = billing::calculate_proration(
        current_sub.as_ref(),
        plan.price_stars,
        plan.period_days,
        Utc::now(),
    );

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
    let Some(from) = sender(&msg) else {
        return Ok(());
    };
    let caller = Caller::resolve(&pool, from).await?;
    let msgs = caller.msgs;
    let charge_id = payment.telegram_payment_charge_id.0.as_str();
    let amount = payment.total_amount as i32;

    let Some((plan_id, payload_user_id)) = billing::parse_invoice_payload(&payment.invoice_payload)
    else {
        // Only our own invoices reach this bot, so this cannot be attributed to a plan or
        // recorded against one; the charge id in the log is all an admin has to go on.
        error!(
            "Charge {charge_id}: invalid invoice payload {:?}",
            payment.invoice_payload
        );
        bot.send_message(msg.chat.id, msgs.buy_error).await?;
        return Ok(());
    };

    let Some(user) = caller.user else {
        error!(
            "Charge {charge_id}: payer telegram_id={} has no account",
            caller.telegram_id
        );
        bot.send_message(msg.chat.id, msgs.buy_error).await?;
        return Ok(());
    };
    let user_id = user.id;

    // The plan was purchasable when the invoice was issued (pre-checkout checked it seconds
    // ago); a NULL here means it was edited in between, which is a failure to record, not a
    // default to guess.
    let plan = sqlx::query!(
        r#"SELECT display_name, price_stars AS "price_stars!", period_days AS "period_days!"
           FROM plans WHERE id = $1 AND price_stars IS NOT NULL AND period_days IS NOT NULL"#,
        plan_id,
    )
    .fetch_optional(&pool)
    .await?;
    // The invoice was issued for `price − credit`, so the credit actually applied is the
    // difference between the plan price and what Telegram charged — not a fresh proration,
    // which would have drifted by the seconds between invoice and payment.
    let credit_amount = plan.as_ref().map_or(0, |p| (p.price_stars - amount).max(0));

    // The Stars are already charged: a fulfilment failure is kept on record and an admin is
    // woken up, instead of letting the only trace of it scroll away in a log.
    let unfulfilled = async |reason: String| {
        error!("Charge {charge_id} from user {user_id} could not be fulfilled: {reason}");
        let recorded = billing::record_failed_payment(
            &pool,
            billing::FailedPayment {
                user_id,
                plan_id,
                telegram_charge_id: charge_id,
                invoice_payload: &payment.invoice_payload,
                amount,
                credit_amount,
                reason: &reason,
            },
        )
        .await;
        if let Err(e) = recorded {
            error!("Charge {charge_id}: could not record the failed payment either: {e}");
        }
        crate::bot::notifications::alert_admins(
            &bot,
            &pool,
            &format!(
                "⚠️ Payment {charge_id} ({amount} ⭐, plan {plan_id}) from user {user_id} was \
                 charged but could not be fulfilled: {reason}\nIt is recorded as a failed \
                 payment; please resolve it by hand."
            ),
        )
        .await;
        bot.send_message(msg.chat.id, msgs.buy_error).await
    };

    if user_id != payload_user_id {
        unfulfilled(format!(
            "invoice was issued to user {payload_user_id}, paid by user {user_id}"
        ))
        .await?;
        return Ok(());
    }
    let Some(plan) = plan else {
        unfulfilled(format!("plan {plan_id} is no longer purchasable")).await?;
        return Ok(());
    };

    let completed = billing::complete_payment(
        &pool,
        billing::CompletePaymentParams {
            user_id,
            plan_id,
            period_days: plan.period_days,
            telegram_charge_id: charge_id,
            invoice_payload: &payment.invoice_payload,
            amount,
            credit_amount,
        },
    )
    .await;
    match completed {
        Ok(outcome) => {
            let message = i18n::format_buy_success(
                msgs,
                &plan.display_name,
                &format_expiry(outcome.expires_at),
            );
            bot.send_message(msg.chat.id, message).await?;
        }
        // Telegram re-delivered the update; the first delivery already did the work.
        Err(billing::PurchaseError::AlreadyProcessed { .. }) => {
            bot.send_message(msg.chat.id, msgs.buy_success).await?;
        }
        Err(e) => {
            unfulfilled(e.to_string()).await?;
        }
    }

    Ok(())
}

/// Date shown to the user for a subscription end.
fn format_expiry(expires_at: chrono::DateTime<Utc>) -> String {
    expires_at.format("%Y-%m-%d").to_string()
}

async fn fallback(bot: Bot, msg: Message, pool: DbPool) -> HandlerResult {
    let Some(from) = sender(&msg) else {
        return Ok(());
    };
    let msgs = Caller::resolve(&pool, from).await?.msgs;
    bot.send_message(msg.chat.id, i18n::format_unknown_message(msgs))
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Command;
    use crate::bot::i18n;
    use floppa_core::models::Lang;
    use teloxide::utils::command::BotCommands;

    // The localized menu table and the parsed enum must describe the same commands.
    #[test]
    fn menu_table_matches_the_command_enum() {
        for lang in [Lang::En, Lang::Ru] {
            let menu = i18n::bot_commands(i18n::for_lang(lang));
            assert_eq!(menu.len(), 5, "one entry per Command variant");
            for entry in &menu {
                assert!(
                    Command::parse(&format!("/{}", entry.command), "floppabot").is_ok(),
                    "/{} is in the {lang} menu but is not a Command",
                    entry.command
                );
                assert!(!entry.description.is_empty());
            }
            let fallback = i18n::format_unknown_message(i18n::for_lang(lang));
            for entry in &menu {
                assert!(fallback.contains(&format!("/{} — ", entry.command)));
            }
        }
    }

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
}
