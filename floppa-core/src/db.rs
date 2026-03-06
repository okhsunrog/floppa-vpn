use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

pub type DbPool = PgPool;

pub async fn init_pool(database_url: &str) -> Result<DbPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await
}

/// Run embedded migrations
pub async fn run_migrations(pool: &DbPool) -> Result<(), sqlx::migrate::MigrateError> {
    sqlx::migrate!("../migrations").run(pool).await
}
