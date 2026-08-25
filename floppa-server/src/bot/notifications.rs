//! Background task that sends subscription expiry notifications via the Telegram bot.

use crate::bot::callback::CallbackAction;
use crate::bot::i18n;
use floppa_core::models::{Lang, NotificationKind};
use floppa_core::{Config, DbPool, billing};
use sqlx::postgres::types::PgInterval;
use std::time::Duration;
use teloxide::prelude::*;
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup};
use tracing::{error, info, warn};

/// A notice that is due: which subscription, whom to tell, and what about.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingNotification {
    subscription_id: i64,
    user_id: i64,
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

/// Subscriptions shorter than this never get the "expires tomorrow" notice: a trial that lasts
/// hours would otherwise be announced as ending "tomorrow" the moment it starts.
const MIN_DURATION_FOR_ADVANCE_NOTICE: PgInterval = PgInterval {
    months: 0,
    days: 2,
    microseconds: 0,
};

/// Every notice that is due and has not been sent yet.
///
/// The window is ±25 h around now: "expires within the next 25 hours" yields the
/// `expiry_1d_before` notice (the loop runs every 30 min, so it fires roughly a day ahead),
/// "expired within the last 25 hours" yields `expiry_now` — wide enough to survive a restart
/// without losing a notice, while `notification_log` keeps each kind to one delivery.
async fn pending_notifications(pool: &DbPool) -> Result<Vec<PendingNotification>, sqlx::Error> {
    sqlx::query_as!(
        PendingNotification,
        r#"
        SELECT
            s.id as subscription_id,
            u.id as user_id,
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
          -- Advance notice only for subscriptions long enough for "tomorrow" to mean something
          AND (s.expires_at <= NOW() OR s.expires_at - s.starts_at >= $3)
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
        MIN_DURATION_FOR_ADVANCE_NOTICE,
    )
    .fetch_all(pool)
    .await
}

/// Remember that `notice` was delivered, so [`pending_notifications`] stops returning it.
async fn record_notification(
    pool: &DbPool,
    notice: &PendingNotification,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO notification_log (user_id, kind, subscription_id) VALUES ($1, $2, $3)",
        notice.user_id,
        notice.kind as _,
        notice.subscription_id,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// One pass: send every due notice and record the ones that got through.
async fn check_and_notify(pool: &DbPool, bot: &Bot, config: &Config) -> anyhow::Result<()> {
    let rows = pending_notifications(pool).await?;

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
                if let Err(e) = record_notification(pool, row).await {
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

#[cfg(test)]
mod tests {
    use super::*;

    async fn seed_user(pool: &DbPool, telegram_id: Option<i64>) -> i64 {
        sqlx::query_scalar!(
            "INSERT INTO users (telegram_id, username) VALUES ($1, 'n') RETURNING id",
            telegram_id
        )
        .fetch_one(pool)
        .await
        .unwrap()
    }

    /// A subscription that started `started_ago` and ends `ends_in` (either may be negative).
    async fn seed_subscription(
        pool: &DbPool,
        user_id: i64,
        started_ago: PgInterval,
        ends_in: PgInterval,
    ) -> i64 {
        sqlx::query_scalar!(
            "INSERT INTO subscriptions (user_id, plan_id, starts_at, expires_at)
             SELECT $1, id, NOW() - $2::interval, NOW() + $3::interval
             FROM plans WHERE name = 'basic'
             RETURNING id",
            user_id,
            started_ago,
            ends_in,
        )
        .fetch_one(pool)
        .await
        .unwrap()
    }

    fn hours(h: i64) -> PgInterval {
        PgInterval {
            months: 0,
            days: 0,
            microseconds: h * 3_600_000_000,
        }
    }

    async fn pending_kinds(pool: &DbPool) -> Vec<(i64, NotificationKind)> {
        let mut kinds: Vec<_> = pending_notifications(pool)
            .await
            .unwrap()
            .into_iter()
            .map(|n| (n.subscription_id, n.kind))
            .collect();
        kinds.sort_by_key(|(id, _)| *id);
        kinds
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn advance_notice_fires_once_within_the_window(pool: DbPool) {
        let user = seed_user(&pool, Some(1001)).await;
        // A month-long subscription with 10 h left: inside the 25 h window.
        let sub = seed_subscription(&pool, user, hours(29 * 24), hours(10)).await;
        // Another month-long subscription with 3 days left: not yet.
        let later = seed_user(&pool, Some(1002)).await;
        seed_subscription(&pool, later, hours(27 * 24), hours(72)).await;

        let due = pending_notifications(&pool).await.unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].subscription_id, sub);
        assert_eq!(due[0].user_id, user);
        assert_eq!(due[0].telegram_id, 1001);
        assert_eq!(due[0].kind, NotificationKind::ExpiryOneDayBefore);

        // Recording it makes the next pass quiet — the dedup that keeps a 30-minute loop from
        // nagging every half hour.
        record_notification(&pool, &due[0]).await.unwrap();
        assert!(pending_kinds(&pool).await.is_empty());
        // The unique (subscription, kind) index rejects a second record outright.
        assert!(record_notification(&pool, &due[0]).await.is_err());
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn expiry_notice_is_separate_from_advance_notice(pool: DbPool) {
        let user = seed_user(&pool, Some(1001)).await;
        // Expired an hour ago: the "expired" notice is due even though the advance one was
        // never recorded (e.g. the server was down), and only that one.
        let sub = seed_subscription(&pool, user, hours(30 * 24), hours(-1)).await;
        assert_eq!(
            pending_kinds(&pool).await,
            vec![(sub, NotificationKind::ExpiryNow)]
        );
        // Expired two days ago: outside the window, silently skipped rather than nagged.
        let stale = seed_user(&pool, Some(1002)).await;
        seed_subscription(&pool, stale, hours(32 * 24), hours(-48)).await;
        assert_eq!(
            pending_kinds(&pool).await,
            vec![(sub, NotificationKind::ExpiryNow)]
        );
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn short_subscriptions_skip_the_advance_notice(pool: DbPool) {
        let user = seed_user(&pool, Some(1001)).await;
        // A 90-minute trial that just started would otherwise be "expiring tomorrow" at once.
        let sub = seed_subscription(&pool, user, hours(0), hours(1)).await;
        assert!(pending_kinds(&pool).await.is_empty());

        // Once it has expired, the expiry notice is still due.
        sqlx::query!(
            "UPDATE subscriptions SET expires_at = NOW() - INTERVAL '5 minutes' WHERE id = $1",
            sub
        )
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(
            pending_kinds(&pool).await,
            vec![(sub, NotificationKind::ExpiryNow)]
        );
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn superseded_and_unreachable_subscriptions_are_skipped(pool: DbPool) {
        // The old subscription is ending, but the user already bought a newer one.
        let renewed = seed_user(&pool, Some(1001)).await;
        seed_subscription(&pool, renewed, hours(29 * 24), hours(10)).await;
        seed_subscription(&pool, renewed, hours(0), hours(30 * 24)).await;

        // Credential-only account: nowhere to send a Telegram message.
        let no_telegram = seed_user(&pool, None).await;
        seed_subscription(&pool, no_telegram, hours(29 * 24), hours(10)).await;

        assert!(pending_kinds(&pool).await.is_empty());
    }
}
