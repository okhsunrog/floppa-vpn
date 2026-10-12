//! Paid events and administrator grants are separate ledgers. Never infer a payment
//! from a subscription source: a credit-funded switch also has source `purchase`.
use super::AppState;
use crate::admin::{
    auth::AdminUser,
    error::ApiError,
    vm_client::{self, TrafficMetric},
};
use axum::{
    Json,
    extract::{Query, State},
};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use floppa_core::DbPool;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

#[derive(Deserialize, IntoParams)]
pub struct AnalyticsQuery {
    /// Inclusive UTC calendar date.
    from: NaiveDate,
    /// Inclusive UTC calendar date, no later than today.
    to: NaiveDate,
}

impl AnalyticsQuery {
    fn bounds(&self) -> Result<(DateTime<Utc>, DateTime<Utc>), ApiError> {
        let days = (self.to - self.from).num_days();
        if !(0..366).contains(&days) || self.to > Utc::now().date_naive() {
            return Err(ApiError::bad_request(
                "Choose 1–366 days, ending no later than today",
            ));
        }
        Ok((
            self.from.and_hms_opt(0, 0, 0).unwrap().and_utc(),
            (self.to + Duration::days(1))
                .and_hms_opt(0, 0, 0)
                .unwrap()
                .and_utc(),
        ))
    }
}

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct AnalyticsEvent {
    pub id: String,
    pub user_id: i64,
    pub name: String,
    pub plan: String,
    pub at: DateTime<Utc>,
    /// first, repeat, credit, admin_grant, trial, or taster.
    pub kind: String,
    /// New Stars received; proration credit is excluded.
    pub amount: i32,
}

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct AnalyticsAccess {
    pub user_id: i64,
    pub name: String,
    /// Source of the currently active subscription; null means no active subscription.
    pub source: Option<String>,
    pub plan: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct AnalyticsUsage {
    pub user_id: i64,
    pub download: i64,
    pub upload: i64,
}

#[derive(Serialize, ToSchema)]
pub struct Analytics {
    pub events: Vec<AnalyticsEvent>,
    pub access: Vec<AnalyticsAccess>,
    /// None means telemetry is unavailable (including an empty metrics store), not zero use.
    pub usage: Option<Vec<AnalyticsUsage>>,
}

const EVENTS_SQL: &str = r#"
WITH paid AS (
    SELECT p.*, row_number() OVER (PARTITION BY user_id ORDER BY completed_at, id) AS purchase_number
    FROM payments p WHERE status = 'completed' AND amount > 0 AND completed_at IS NOT NULL
), events AS (
    SELECT 'payment-' || id AS id, user_id, plan_id, completed_at AS at,
           CASE WHEN purchase_number = 1 THEN 'first' ELSE 'repeat' END AS kind, amount
    FROM paid WHERE completed_at >= $1 AND completed_at < $2
    UNION ALL
    SELECT 'payment-' || id, user_id, plan_id, completed_at, 'credit', 0
    FROM payments WHERE status = 'completed' AND amount = 0 AND credit_amount > 0
      AND completed_at >= $1 AND completed_at < $2
    UNION ALL
    SELECT 'subscription-' || id, user_id, plan_id, starts_at, source, 0
    FROM subscriptions WHERE source IN ('admin_grant', 'trial', 'taster')
      AND starts_at >= $1 AND starts_at < $2
)
SELECT e.id, e.user_id, COALESCE(NULLIF(CONCAT_WS(' ', u.first_name, u.last_name), ''), u.username, '#' || u.id) AS name,
       p.display_name AS plan, e.at, e.kind, e.amount
FROM events e JOIN users u ON u.id = e.user_id JOIN plans p ON p.id = e.plan_id
ORDER BY e.at DESC, e.id
"#;

async fn events(
    pool: &DbPool,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<AnalyticsEvent>, sqlx::Error> {
    sqlx::query_as(EVENTS_SQL)
        .bind(from)
        .bind(to)
        .fetch_all(pool)
        .await
}

#[utoipa::path(get, path = "/analytics", tag = "admin", security(("bearer" = [])),
    params(AnalyticsQuery), responses((status = 200, body = Analytics), (status = 400, body = ApiError),
    (status = 401, body = ApiError), (status = 403, body = ApiError)))]
pub(super) async fn get_analytics(
    _admin: AdminUser,
    State(state): State<AppState>,
    Query(query): Query<AnalyticsQuery>,
) -> Result<Json<Analytics>, ApiError> {
    let (from, to) = query.bounds()?;
    let events = events(&state.pool, from, to).await?;
    let access = sqlx::query_as::<_, AnalyticsAccess>(r#"
        SELECT u.id AS user_id, COALESCE(NULLIF(CONCAT_WS(' ',u.first_name,u.last_name),''),u.username,'#'||u.id) AS name,
               cs.source, cs.plan_display_name AS plan
        FROM users u LEFT JOIN current_subscriptions cs ON cs.user_id = u.id AND cs.is_active
        ORDER BY u.id
    "#).fetch_all(&state.pool).await?;
    let end = to.min(Utc::now());
    let (wg, vless) = tokio::join!(
        state.vm.traffic_between(TrafficMetric::Wg, from, end),
        state.vm.traffic_between(TrafficMetric::Vless, from, end),
    );
    let usage = match (
        vm_client::logged("analytics_wg", wg),
        vm_client::logged("analytics_vless", vless),
    ) {
        (Some(mut wg), Some(vless)) if !wg.is_empty() || !vless.is_empty() => {
            for (id, traffic) in vless {
                let entry = wg.entry(id).or_default();
                entry.download += traffic.download;
                entry.upload += traffic.upload;
            }
            let mut usage: Vec<_> = wg
                .into_iter()
                .map(|(user_id, t)| AnalyticsUsage {
                    user_id,
                    download: t.download,
                    upload: t.upload,
                })
                .collect();
            usage.sort_by_key(|u| u.user_id);
            Some(usage)
        }
        _ => None,
    };
    Ok(Json(Analytics {
        events,
        access,
        usage,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_calendar_window() {
        let today = Utc::now().date_naive();
        assert!(
            AnalyticsQuery {
                from: today,
                to: today
            }
            .bounds()
            .is_ok()
        );
        assert!(
            AnalyticsQuery {
                from: today,
                to: today - Duration::days(1)
            }
            .bounds()
            .is_err()
        );
        assert!(
            AnalyticsQuery {
                from: today - Duration::days(366),
                to: today
            }
            .bounds()
            .is_err()
        );
        assert!(
            AnalyticsQuery {
                from: today,
                to: today + Duration::days(1)
            }
            .bounds()
            .is_err()
        );
    }

    #[sqlx::test(migrations = "../migrations")]
    async fn counts_actual_payments_without_losing_previous_history(pool: DbPool) {
        let user: i64 = sqlx::query_scalar(
            "INSERT INTO users (username) VALUES ('analytics-test') RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let plan: i32 = sqlx::query_scalar("SELECT id FROM plans LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
        let from = Utc::now() - Duration::days(30);
        for (days, amount, credit, status) in [
            (60, 150, 0, "completed"),
            (5, 100, 50, "completed"),
            (4, 0, 150, "completed"),
            (3, 500, 0, "failed"),
        ] {
            sqlx::query("INSERT INTO payments (user_id,plan_id,amount,credit_amount,status,invoice_payload,completed_at) VALUES ($1,$2,$3,$4,$5,'test',$6)")
                .bind(user).bind(plan).bind(amount).bind(credit).bind(status).bind(Utc::now()-Duration::days(days)).execute(&pool).await.unwrap();
        }
        sqlx::query("INSERT INTO subscriptions (user_id,plan_id,starts_at,source) VALUES ($1,$2,NOW(),'admin_grant')").bind(user).bind(plan).execute(&pool).await.unwrap();
        let rows = events(&pool, from, Utc::now() + Duration::days(1))
            .await
            .unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows.iter().filter(|r| r.kind == "repeat").count(), 1);
        assert_eq!(rows.iter().filter(|r| r.kind == "first").count(), 0);
        assert_eq!(rows.iter().filter(|r| r.kind == "admin_grant").count(), 1);
        assert_eq!(rows.iter().filter(|r| r.kind == "credit").count(), 1);
        assert_eq!(rows.iter().map(|r| r.amount).sum::<i32>(), 100);
    }
}
