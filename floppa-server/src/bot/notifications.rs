//! Background task that sends subscription expiry notifications via the Telegram bot.

use crate::bot::i18n;
use floppa_core::{Config, DbPool, billing};
use std::time::Duration;
use teloxide::prelude::*;
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup};
use tracing::{error, info, warn};

/// Row returned by the expiry notification query.
struct ExpiringSubscription {
    subscription_id: i64,
    telegram_id: i64,
    language: Option<String>,
    /// "expiry_1d_before" or "expiry_now"
    kind: String,
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

async fn check_and_notify(pool: &DbPool, bot: &Bot, config: &Config) -> anyhow::Result<()> {
    // Find subscriptions expiring within 24-25h (1 day before) or already expired within last 1h,
    // that haven't been notified yet.
    let rows = sqlx::query_as!(
        ExpiringSubscription,
        r#"
        SELECT
            s.id as subscription_id,
            u.telegram_id,
            u.language,
            CASE
                WHEN s.expires_at <= NOW() THEN 'expiry_now'
                ELSE 'expiry_1d_before'
            END as "kind!"
        FROM subscriptions s
        JOIN users u ON s.user_id = u.id
        WHERE s.expires_at IS NOT NULL
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
                    WHEN s.expires_at <= NOW() THEN 'expiry_now'
                    ELSE 'expiry_1d_before'
                END
          )
        "#,
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
        let msgs = i18n::for_lang(row.language.as_deref());

        let header = match row.kind.as_str() {
            "expiry_now" => msgs.notify_expired,
            _ => msgs.notify_expires_tomorrow,
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
                    format!("buy:{}", p.id),
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
                    row.kind,
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
