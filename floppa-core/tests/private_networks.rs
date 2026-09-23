//! The invariants `0024_private_networks.sql` enforces: a plan that grants a private network is
//! never public and is only ever an administrator's current subscription.

use sqlx::PgPool;

async fn plan(pool: &PgPool, name: &str, is_public: bool) -> i32 {
    sqlx::query_scalar(
        "INSERT INTO plans (name, display_name, is_public) VALUES ($1, $1, $2) RETURNING id",
    )
    .bind(name)
    .bind(is_public)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn user(pool: &PgPool, telegram_id: i64, is_admin: bool) -> i64 {
    sqlx::query_scalar(
        "INSERT INTO users (telegram_id, username, is_admin) VALUES ($1, 'u', $2) RETURNING id",
    )
    .bind(telegram_id)
    .bind(is_admin)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn subscribe(pool: &PgPool, user_id: i64, plan_id: i32) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO subscriptions (user_id, plan_id, starts_at, is_current) VALUES ($1, $2, NOW(), true)",
    )
    .bind(user_id)
    .bind(plan_id)
    .execute(pool)
    .await
    .map(drop)
}

async fn network(pool: &PgPool, id: &str, cidrs: &[&str]) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO private_networks (id, display_name, cidrs) VALUES ($1, $1, $2::cidr[])",
    )
    .bind(id)
    .bind(cidrs.iter().map(|c| c.to_string()).collect::<Vec<_>>())
    .execute(pool)
    .await
    .map(drop)
}

async fn link(pool: &PgPool, plan_id: i32, network_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO plan_private_networks (plan_id, network_id) VALUES ($1, $2)")
        .bind(plan_id)
        .bind(network_id)
        .execute(pool)
        .await
        .map(drop)
}

/// The constraint name a trigger refused with.
fn refused_by(result: Result<(), sqlx::Error>) -> String {
    let err = result.expect_err("the write should have been refused");
    err.as_database_error()
        .and_then(|db| db.constraint().map(str::to_owned))
        .unwrap_or_else(|| panic!("not a named constraint violation: {err:?}"))
}

#[sqlx::test(migrations = "../migrations")]
async fn a_public_plan_cannot_carry_a_network(pool: PgPool) {
    network(&pool, "home", &["10.66.66.0/24"]).await.unwrap();
    let public = plan(&pool, "pub", true).await;
    assert_eq!(
        refused_by(link(&pool, public, "home").await),
        "private_network_plan_not_public"
    );
}

#[sqlx::test(migrations = "../migrations")]
async fn a_plan_with_a_network_cannot_be_made_public(pool: PgPool) {
    network(&pool, "home", &["10.66.66.0/24"]).await.unwrap();
    let private = plan(&pool, "priv", false).await;
    link(&pool, private, "home").await.unwrap();
    let made_public = sqlx::query("UPDATE plans SET is_public = true WHERE id = $1")
        .bind(private)
        .execute(&pool)
        .await
        .map(drop);
    assert_eq!(refused_by(made_public), "private_network_plan_not_public");
}

#[sqlx::test(migrations = "../migrations")]
async fn only_an_administrator_is_subscribed_to_such_a_plan(pool: PgPool) {
    network(&pool, "home", &["10.66.66.0/24"]).await.unwrap();
    let private = plan(&pool, "priv", false).await;
    link(&pool, private, "home").await.unwrap();

    let admin = user(&pool, 1, true).await;
    subscribe(&pool, admin, private).await.unwrap();

    let someone = user(&pool, 2, false).await;
    assert_eq!(
        refused_by(subscribe(&pool, someone, private).await),
        "private_network_admin_only"
    );
}

#[sqlx::test(migrations = "../migrations")]
async fn a_network_cannot_be_linked_under_someone_already_on_the_plan(pool: PgPool) {
    network(&pool, "home", &["10.66.66.0/24"]).await.unwrap();
    let private = plan(&pool, "priv", false).await;
    let someone = user(&pool, 2, false).await;
    subscribe(&pool, someone, private).await.unwrap();
    assert_eq!(
        refused_by(link(&pool, private, "home").await),
        "private_network_admin_only"
    );
}

#[sqlx::test(migrations = "../migrations")]
async fn an_administrator_on_such_a_plan_keeps_the_flag(pool: PgPool) {
    network(&pool, "home", &["10.66.66.0/24"]).await.unwrap();
    let private = plan(&pool, "priv", false).await;
    link(&pool, private, "home").await.unwrap();
    let admin = user(&pool, 1, true).await;
    subscribe(&pool, admin, private).await.unwrap();

    let demoted = sqlx::query("UPDATE users SET is_admin = false WHERE id = $1")
        .bind(admin)
        .execute(&pool)
        .await
        .map(drop);
    assert_eq!(refused_by(demoted), "private_network_admin_only");
}

#[sqlx::test(migrations = "../migrations")]
async fn networks_are_ipv4_and_not_empty(pool: PgPool) {
    assert!(network(&pool, "v6", &["fd00::/8"]).await.is_err());
    assert!(network(&pool, "none", &[]).await.is_err());
    network(&pool, "ok", &["10.66.66.0/24", "10.65.65.0/24"])
        .await
        .unwrap();
}
