//! Billing service: proration, payment processing, subscription switching.

use crate::DbPool;
use crate::error::Result;
use chrono::{Duration, Utc};

/// A plan available for purchase with Stars.
pub struct PurchasablePlan {
    pub id: i32,
    pub display_name: String,
    pub price_stars: i32,
    pub period_days: i32,
    pub default_speed_limit_mbps: Option<i32>,
    pub max_peers: i32,
}

/// Current subscription info needed for proration.
pub struct CurrentSubscription {
    pub subscription_id: i64,
    pub plan_id: i32,
    pub price_stars: Option<i32>,
    pub period_days: Option<i32>,
    pub expires_at: Option<chrono::DateTime<Utc>>,
}

/// Result of proration calculation.
pub struct ProrationResult {
    /// Credit in Stars from remaining subscription value.
    pub credit_stars: i32,
    /// Amount the user must pay (new plan price minus credit, min 0).
    pub payable_stars: i32,
    /// Subscription duration in days (extended when credit covers the full price).
    pub subscription_days: i32,
}

/// Get plans that can be purchased with Stars.
pub async fn get_purchasable_plans(pool: &DbPool) -> Result<Vec<PurchasablePlan>> {
    let rows = sqlx::query!(
        r#"
        SELECT id, display_name, price_stars as "price_stars!", period_days as "period_days!",
               default_speed_limit_mbps, max_peers
        FROM plans
        WHERE price_stars IS NOT NULL AND period_days IS NOT NULL AND is_public = true
        ORDER BY price_stars ASC
        "#,
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| PurchasablePlan {
            id: r.id,
            display_name: r.display_name,
            price_stars: r.price_stars,
            period_days: r.period_days,
            default_speed_limit_mbps: r.default_speed_limit_mbps,
            max_peers: r.max_peers,
        })
        .collect())
}

/// Get the user's current active subscription with plan pricing for proration.
pub async fn get_current_subscription(
    pool: &DbPool,
    user_id: i64,
) -> Result<Option<CurrentSubscription>> {
    let row = sqlx::query!(
        r#"
        SELECT s.id, s.plan_id, p.price_stars, p.period_days, s.expires_at
        FROM subscriptions s
        JOIN plans p ON s.plan_id = p.id
        WHERE s.user_id = $1 AND (s.expires_at IS NULL OR s.expires_at > NOW())
        ORDER BY s.expires_at DESC NULLS FIRST
        LIMIT 1
        "#,
        user_id,
    )
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| CurrentSubscription {
        subscription_id: r.id,
        plan_id: r.plan_id,
        price_stars: r.price_stars,
        period_days: r.period_days,
        expires_at: r.expires_at,
    }))
}

/// Calculate proration credit and payable amount.
///
/// Only paid plans with a finite expiry date generate credit.
/// Trial, free, and permanent subscriptions give zero credit.
pub fn calculate_proration(
    current_sub: Option<&CurrentSubscription>,
    new_plan_price_stars: i32,
    new_plan_period_days: i32,
) -> ProrationResult {
    let credit_stars = match current_sub {
        Some(sub) => match (sub.price_stars, sub.period_days, sub.expires_at) {
            (Some(price), Some(period), Some(expires)) if price > 0 && period > 0 => {
                let now = Utc::now();
                let remaining_days = (expires - now).num_days().max(0);
                (price as i64 * remaining_days / period as i64) as i32
            }
            _ => 0,
        },
        None => 0,
    };

    let payable_stars = (new_plan_price_stars - credit_stars).max(0);

    // When credit covers the full price, convert it into proportional extra days
    let subscription_days = if payable_stars == 0 && new_plan_price_stars > 0 {
        (credit_stars as i64 * new_plan_period_days as i64 / new_plan_price_stars as i64) as i32
    } else {
        new_plan_period_days
    };

    ProrationResult {
        credit_stars,
        payable_stars,
        subscription_days,
    }
}

/// Build an invoice payload encoding plan ID and user ID for verification.
pub fn build_invoice_payload(plan_id: i32, user_id: i64) -> String {
    let ts = Utc::now().timestamp();
    format!("plan:{plan_id}:user:{user_id}:{ts}")
}

/// Parse plan_id and user_id from an invoice payload. Returns None if invalid format.
pub fn parse_invoice_payload(payload: &str) -> Option<(i32, i64)> {
    let parts: Vec<&str> = payload.split(':').collect();
    if parts.len() >= 4 && parts[0] == "plan" && parts[2] == "user" {
        let plan_id = parts[1].parse().ok()?;
        let user_id = parts[3].parse().ok()?;
        Some((plan_id, user_id))
    } else {
        None
    }
}

/// Parameters for completing a payment.
pub struct CompletePaymentParams<'a> {
    pub user_id: i64,
    pub plan_id: i32,
    pub period_days: i32,
    pub telegram_charge_id: &'a str,
    pub invoice_payload: &'a str,
    pub amount: i32,
    pub credit_amount: i32,
}

/// Complete a Stars payment: expire old sub, create new sub, record payment.
///
/// Idempotent on `telegram_charge_id` (UNIQUE constraint in DB).
pub async fn complete_payment(pool: &DbPool, params: CompletePaymentParams<'_>) -> Result<i64> {
    let now = Utc::now();
    let expires_at = now + Duration::days(params.period_days as i64);

    let mut tx = pool.begin().await?;

    // Expire current active subscription
    sqlx::query!(
        "UPDATE subscriptions SET expires_at = NOW() WHERE user_id = $1 AND (expires_at IS NULL OR expires_at > NOW())",
        params.user_id,
    )
    .execute(&mut *tx)
    .await?;

    // Create new subscription
    let sub_id: i64 = sqlx::query_scalar!(
        r#"INSERT INTO subscriptions (user_id, plan_id, starts_at, expires_at, source) VALUES ($1, $2, $3, $4, 'purchase') RETURNING id as "id!""#,
        params.user_id,
        params.plan_id,
        now,
        expires_at,
    )
    .fetch_one(&mut *tx)
    .await?;

    // Record payment (telegram_charge_id UNIQUE enforces idempotency)
    sqlx::query!(
        r#"
        INSERT INTO payments (user_id, plan_id, amount, credit_amount, invoice_payload, telegram_charge_id, subscription_id, status, completed_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, 'completed', NOW())
        "#,
        params.user_id,
        params.plan_id,
        params.amount,
        params.credit_amount,
        params.invoice_payload,
        params.telegram_charge_id,
        sub_id,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(sub_id)
}

/// Process a credit-funded plan switch (proration credit covers the full price).
///
/// Idempotent: skips if a zero-amount payment for the same user+plan was
/// completed within the last minute (guards against duplicate callbacks).
pub async fn process_credit_switch(
    pool: &DbPool,
    user_id: i64,
    plan_id: i32,
    subscription_days: i32,
    credit_amount: i32,
) -> Result<Option<i64>> {
    // Dedup: skip if a recent credit switch for the same user+plan exists
    let recent = sqlx::query_scalar!(
        r#"
        SELECT 1 as "x!" FROM payments
        WHERE user_id = $1 AND plan_id = $2 AND amount = 0 AND status = 'completed'
          AND completed_at > NOW() - INTERVAL '1 minute'
        LIMIT 1
        "#,
        user_id,
        plan_id,
    )
    .fetch_optional(pool)
    .await?;

    if recent.is_some() {
        return Ok(None);
    }

    let now = Utc::now();
    let expires_at = now + Duration::days(subscription_days as i64);

    let mut tx = pool.begin().await?;

    sqlx::query!(
        "UPDATE subscriptions SET expires_at = NOW() WHERE user_id = $1 AND (expires_at IS NULL OR expires_at > NOW())",
        user_id,
    )
    .execute(&mut *tx)
    .await?;

    let sub_id: i64 = sqlx::query_scalar!(
        r#"INSERT INTO subscriptions (user_id, plan_id, starts_at, expires_at, source) VALUES ($1, $2, $3, $4, 'purchase') RETURNING id as "id!""#,
        user_id,
        plan_id,
        now,
        expires_at,
    )
    .fetch_one(&mut *tx)
    .await?;

    sqlx::query!(
        r#"
        INSERT INTO payments (user_id, plan_id, amount, credit_amount, invoice_payload, subscription_id, status, completed_at)
        VALUES ($1, $2, 0, $3, $4, $5, 'completed', NOW())
        "#,
        user_id,
        plan_id,
        credit_amount,
        format!("credit_switch:plan:{plan_id}:user:{user_id}"),
        sub_id,
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(Some(sub_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DbPool;

    // ── Pure function tests (no DB) ──

    #[test]
    fn test_proration_no_current_sub() {
        let result = calculate_proration(None, 250, 30);
        assert_eq!(result.credit_stars, 0);
        assert_eq!(result.payable_stars, 250);
    }

    #[test]
    fn test_proration_trial_sub_no_credit() {
        let sub = CurrentSubscription {
            subscription_id: 1,
            plan_id: 1,
            price_stars: None, // trial has no price
            period_days: Some(7),
            expires_at: Some(Utc::now() + Duration::days(5)),
        };
        let result = calculate_proration(Some(&sub), 250, 30);
        assert_eq!(result.credit_stars, 0);
        assert_eq!(result.payable_stars, 250);
    }

    #[test]
    fn test_proration_paid_sub_half_remaining() {
        // Add 1 minute buffer to avoid num_days() rounding down due to sub-second timing
        let sub = CurrentSubscription {
            subscription_id: 1,
            plan_id: 1,
            price_stars: Some(300),
            period_days: Some(30),
            expires_at: Some(Utc::now() + Duration::days(15) + Duration::minutes(1)),
        };
        // credit = 300 * 15 / 30 = 150
        let result = calculate_proration(Some(&sub), 250, 30);
        assert_eq!(result.credit_stars, 150);
        assert_eq!(result.payable_stars, 100);
    }

    #[test]
    fn test_proration_credit_exceeds_price() {
        let sub = CurrentSubscription {
            subscription_id: 1,
            plan_id: 2,
            price_stars: Some(300),
            period_days: Some(30),
            expires_at: Some(Utc::now() + Duration::days(28) + Duration::minutes(1)),
        };
        // credit = 300 * 28 / 30 = 280
        let result = calculate_proration(Some(&sub), 100, 30);
        assert_eq!(result.credit_stars, 280);
        assert_eq!(result.payable_stars, 0); // clamped to 0
        // 280 credit buys 280 * 30 / 100 = 84 days of the cheaper plan
        assert_eq!(result.subscription_days, 84);
    }

    #[test]
    fn test_proration_permanent_sub_no_credit() {
        let sub = CurrentSubscription {
            subscription_id: 1,
            plan_id: 1,
            price_stars: Some(100),
            period_days: Some(30),
            expires_at: None, // permanent
        };
        let result = calculate_proration(Some(&sub), 250, 30);
        assert_eq!(result.credit_stars, 0);
        assert_eq!(result.payable_stars, 250);
    }

    #[test]
    fn test_proration_expired_sub_no_credit() {
        let sub = CurrentSubscription {
            subscription_id: 1,
            plan_id: 1,
            price_stars: Some(100),
            period_days: Some(30),
            expires_at: Some(Utc::now() - Duration::days(1)),
        };
        let result = calculate_proration(Some(&sub), 250, 30);
        assert_eq!(result.credit_stars, 0);
        assert_eq!(result.payable_stars, 250);
    }

    #[test]
    fn test_proration_free_plan_no_credit() {
        let sub = CurrentSubscription {
            subscription_id: 1,
            plan_id: 1,
            price_stars: Some(0), // free plan
            period_days: Some(30),
            expires_at: Some(Utc::now() + Duration::days(15)),
        };
        let result = calculate_proration(Some(&sub), 250, 30);
        assert_eq!(result.credit_stars, 0);
        assert_eq!(result.payable_stars, 250);
    }

    #[test]
    fn test_invoice_payload_roundtrip() {
        let payload = build_invoice_payload(42, 999);
        assert!(payload.starts_with("plan:42:user:999:"));
        assert_eq!(parse_invoice_payload(&payload), Some((42, 999)));
    }

    #[test]
    fn test_parse_invalid_payload() {
        assert_eq!(parse_invoice_payload("garbage"), None);
        assert_eq!(parse_invoice_payload("plan:abc:user:1:0"), None);
        assert_eq!(parse_invoice_payload("plan:1:0"), None); // old format
        assert_eq!(parse_invoice_payload(""), None);
    }

    // ── Database integration tests ──

    async fn seed_user(pool: &DbPool, telegram_id: i64) -> i64 {
        sqlx::query_scalar!(
            "INSERT INTO users (telegram_id, username) VALUES ($1, 'testuser') RETURNING id",
            telegram_id,
        )
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn seed_subscription(pool: &DbPool, user_id: i64, plan_id: i32) {
        sqlx::query!(
            "INSERT INTO subscriptions (user_id, plan_id, starts_at) VALUES ($1, $2, NOW())",
            user_id,
            plan_id,
        )
        .execute(pool)
        .await
        .unwrap();
    }

    async fn make_plan_purchasable(pool: &DbPool, name: &str, price_stars: i32, period_days: i32) {
        sqlx::query!(
            "UPDATE plans SET price_stars = $1, period_days = $2 WHERE name = $3",
            price_stars,
            period_days,
            name,
        )
        .execute(pool)
        .await
        .unwrap();
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_get_purchasable_plans_empty_by_default(pool: DbPool) {
        // No plans have price_stars set by default
        let plans = get_purchasable_plans(&pool).await.unwrap();
        assert!(plans.is_empty());
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_get_purchasable_plans_returns_configured(pool: DbPool) {
        make_plan_purchasable(&pool, "standard", 100, 30).await;
        make_plan_purchasable(&pool, "premium", 250, 30).await;

        let plans = get_purchasable_plans(&pool).await.unwrap();
        assert_eq!(plans.len(), 2);
        assert!(plans[0].price_stars <= plans[1].price_stars); // ordered by price
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_get_current_subscription_none(pool: DbPool) {
        let user_id = seed_user(&pool, 11111).await;
        let sub = get_current_subscription(&pool, user_id).await.unwrap();
        assert!(sub.is_none());
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_get_current_subscription_exists(pool: DbPool) {
        let user_id = seed_user(&pool, 11111).await;
        let plan_id = sqlx::query_scalar!("SELECT id FROM plans WHERE name = 'standard'")
            .fetch_one(&pool)
            .await
            .unwrap();
        seed_subscription(&pool, user_id, plan_id).await;

        let sub = get_current_subscription(&pool, user_id).await.unwrap();
        assert!(sub.is_some());
        assert_eq!(sub.unwrap().plan_id, plan_id);
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_complete_payment_creates_subscription(pool: DbPool) {
        let user_id = seed_user(&pool, 11111).await;
        let plan_id = sqlx::query_scalar!("SELECT id FROM plans WHERE name = 'standard'")
            .fetch_one(&pool)
            .await
            .unwrap();

        let sub_id = complete_payment(
            &pool,
            CompletePaymentParams {
                user_id,
                plan_id,
                period_days: 30,
                telegram_charge_id: "charge_123",
                invoice_payload: "plan:1:123",
                amount: 100,
                credit_amount: 0,
            },
        )
        .await
        .unwrap();

        // Verify subscription
        let sub = sqlx::query!(
            "SELECT plan_id, expires_at FROM subscriptions WHERE id = $1",
            sub_id,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(sub.plan_id, plan_id);
        assert!(sub.expires_at.is_some());

        // Verify payment record
        let payment = sqlx::query!(
            "SELECT status, telegram_charge_id, amount, credit_amount FROM payments WHERE subscription_id = $1",
            sub_id,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(payment.status, "completed");
        assert_eq!(payment.telegram_charge_id.as_deref(), Some("charge_123"));
        assert_eq!(payment.amount, 100);
        assert_eq!(payment.credit_amount, 0);
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_complete_payment_idempotent(pool: DbPool) {
        let user_id = seed_user(&pool, 11111).await;
        let plan_id = sqlx::query_scalar!("SELECT id FROM plans WHERE name = 'standard'")
            .fetch_one(&pool)
            .await
            .unwrap();

        // First call succeeds
        complete_payment(
            &pool,
            CompletePaymentParams {
                user_id,
                plan_id,
                period_days: 30,
                telegram_charge_id: "charge_dup",
                invoice_payload: "plan:1:1",
                amount: 100,
                credit_amount: 0,
            },
        )
        .await
        .unwrap();

        // Second call with same charge_id fails (UNIQUE constraint)
        let result = complete_payment(
            &pool,
            CompletePaymentParams {
                user_id,
                plan_id,
                period_days: 30,
                telegram_charge_id: "charge_dup",
                invoice_payload: "plan:1:2",
                amount: 100,
                credit_amount: 0,
            },
        )
        .await;
        assert!(result.is_err());
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_upgrade_expires_old_sub(pool: DbPool) {
        let user_id = seed_user(&pool, 11111).await;
        let standard_id = sqlx::query_scalar!("SELECT id FROM plans WHERE name = 'standard'")
            .fetch_one(&pool)
            .await
            .unwrap();
        let premium_id = sqlx::query_scalar!("SELECT id FROM plans WHERE name = 'premium'")
            .fetch_one(&pool)
            .await
            .unwrap();

        seed_subscription(&pool, user_id, standard_id).await;

        // Upgrade to premium
        complete_payment(
            &pool,
            CompletePaymentParams {
                user_id,
                plan_id: premium_id,
                period_days: 30,
                telegram_charge_id: "charge_upgrade",
                invoice_payload: "plan:2:1",
                amount: 250,
                credit_amount: 0,
            },
        )
        .await
        .unwrap();

        // Only one active subscription
        let active_count = sqlx::query_scalar!(
            "SELECT COUNT(*) FROM subscriptions WHERE user_id = $1 AND (expires_at IS NULL OR expires_at > NOW())",
            user_id,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(active_count, Some(1));
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_credit_switch(pool: DbPool) {
        let user_id = seed_user(&pool, 11111).await;
        let plan_id = sqlx::query_scalar!("SELECT id FROM plans WHERE name = 'premium'")
            .fetch_one(&pool)
            .await
            .unwrap();

        let sub_id = process_credit_switch(&pool, user_id, plan_id, 84, 150)
            .await
            .unwrap();
        assert!(sub_id.is_some());

        // Verify zero-amount payment recorded
        let payment = sqlx::query!(
            "SELECT amount, credit_amount, status FROM payments WHERE subscription_id = $1",
            sub_id.unwrap(),
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(payment.amount, 0);
        assert_eq!(payment.credit_amount, 150);
        assert_eq!(payment.status, "completed");
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn test_credit_switch_idempotent(pool: DbPool) {
        let user_id = seed_user(&pool, 11111).await;
        let plan_id = sqlx::query_scalar!("SELECT id FROM plans WHERE name = 'premium'")
            .fetch_one(&pool)
            .await
            .unwrap();

        // First call succeeds
        let first = process_credit_switch(&pool, user_id, plan_id, 84, 150)
            .await
            .unwrap();
        assert!(first.is_some());

        // Second call within 1 minute is skipped (dedup)
        let second = process_credit_switch(&pool, user_id, plan_id, 84, 150)
            .await
            .unwrap();
        assert!(second.is_none());
    }
}
