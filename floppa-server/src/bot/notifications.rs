//! Background task that sends subscription expiry notifications via the Telegram bot.

use crate::bot::callback::CallbackAction;
use crate::bot::i18n;
use floppa_core::models::{Lang, NotificationKind};
use floppa_core::{Config, DbPool, billing};
use std::time::Duration;
use teloxide::prelude::*;
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup};
use tracing::{error, info, warn};

/// Row returned by the expiry notification query.
struct ExpiringSubscription {
    subscription_id: i64,
    telegram_id: i64,
    language: Option<Lang>,
    kind: NotificationKind,
}

/// Spawn the background notification loop. Checks every 30 minutes.
pub fn spawn(pool: DbPool, bot: Bot, config: Config) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_mins(30));
        loop {
            interval.tick().await;
            if let Err(e) = check_and_notify(&pool, &bot, &config).await {
                error!("Notification check failed: {e}");
            }
        }
    });
}

/// Best-effort broadcast to every admin who has a Telegram chat with the bot. Delivery failures
/// (an admin blocked the bot, say) are logged and skipped — the caller has already recorded
/// whatever it is alerting about.
pub async fn alert_admins(bot: &Bot, pool: &DbPool, text: &str) {
    let admins = match sqlx::query_scalar!(
        r#"SELECT telegram_id AS "telegram_id!" FROM users
           WHERE is_admin AND telegram_id IS NOT NULL"#
    )
    .fetch_all(pool)
    .await
    {
        Ok(ids) => ids,
        Err(e) => {
            error!("Could not look up admins to alert: {e}");
            return;
        }
    };
    for telegram_id in admins {
        if let Err(e) = bot.send_message(ChatId(telegram_id), text).await {
            warn!("Could not alert admin telegram_id={telegram_id}: {e}");
        }
    }
}

async fn check_and_notify(pool: &DbPool, bot: &Bot, config: &Config) -> anyhow::Result<()> {
    // Find subscriptions expiring within 24-25h (1 day before) or already expired within last 1h,
    // that haven't been notified yet.
    let rows = sqlx::query_as!(
        ExpiringSubscription,
        r#"
        SELECT
            s.id as subscription_id,
            u.telegram_id as "telegram_id!",
            u.language AS "language: Lang",
            CASE
                WHEN s.expires_at <= NOW() THEN $1
                ELSE $2
            END as "kind!: NotificationKind"
        FROM subscriptions s
        JOIN users u ON s.user_id = u.id
        WHERE s.expires_at IS NOT NULL
          -- Skip credential-only users (no Telegram chat to notify)
          AND u.telegram_id IS NOT NULL
          -- Expires within next 25 hours OR expired within last 25 hours
          AND s.expires_at BETWEEN NOW() - INTERVAL '25 hours' AND NOW() + INTERVAL '25 hours'
          -- No newer subscription for this user
          AND NOT EXISTS (
              SELECT 1 FROM subscriptions s2
              WHERE s2.user_id = s.user_id
                AND s2.id != s.id
                AND (s2.expires_at IS NULL OR s2.expires_at > s.expires_at)
          )
          -- Not already notified with this kind
          AND NOT EXISTS (
              SELECT 1 FROM notification_log nl
              WHERE nl.subscription_id = s.id
                AND nl.kind = CASE
                    WHEN s.expires_at <= NOW() THEN $1
                    ELSE $2
                END
          )
        "#,
        NotificationKind::ExpiryNow as _,
        NotificationKind::ExpiryOneDayBefore as _,
    )
    .fetch_all(pool)
    .await?;

    if rows.is_empty() {
        return Ok(());
    }

    info!("Sending {} expiry notifications", rows.len());

    let plans = billing::get_purchasable_plans(pool).await?;
    let stars_rub_rate = config.bot.as_ref().and_then(|b| b.stars_rub_rate);

    for row in &rows {
        let msgs = i18n::for_lang(row.language.unwrap_or(Lang::En));

        let header = match row.kind {
            NotificationKind::ExpiryNow => msgs.notify_expired,
            NotificationKind::ExpiryOneDayBefore => msgs.notify_expires_tomorrow,
        };

        let text = i18n::format_plans_message(msgs, header, &plans);

        // Build per-user keyboard with localized button labels
        let user_buttons: Vec<Vec<InlineKeyboardButton>> = plans
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
        let keyboard = InlineKeyboardMarkup::new(user_buttons);

        let chat_id = ChatId(row.telegram_id);

        match bot
            .send_message(chat_id, &text)
            .reply_markup(keyboard)
            .await
        {
            Ok(_) => {
                // Record that we sent this notification
                if let Err(e) = sqlx::query!(
                    "INSERT INTO notification_log (user_id, kind, subscription_id)
                     SELECT u.id, $2, $3 FROM users u WHERE u.telegram_id = $1",
                    row.telegram_id,
                    row.kind as _,
                    row.subscription_id,
                )
                .execute(pool)
                .await
                {
                    warn!(
                        "Failed to log notification for telegram_id={}: {e}",
                        row.telegram_id
                    );
                }
            }
            Err(e) => {
                // User may have blocked the bot — log and continue
                warn!(
                    "Failed to send notification to telegram_id={}: {e}",
                    row.telegram_id
                );
            }
        }
    }

    Ok(())
}
