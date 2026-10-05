//! Populate the isolated development database. Never imports production data.
use anyhow::{Result, bail};
use chrono::{Duration, Utc};
use floppa_core::{Secrets, crypto, db, password, services, wg_keys};

#[tokio::main]
async fn main() -> Result<()> {
    let secrets = Secrets::from_env()?;
    let url = url::Url::parse(&secrets.database_url)?;
    if url.path() != "/floppa_dev" || !matches!(url.host_str(), Some("127.0.0.1" | "localhost")) {
        bail!("Development seed requires a loopback database named floppa_dev");
    }
    let pool = db::init_pool(&secrets.database_url).await?;
    db::run_migrations(&pool).await?;
    let hash = password::hash_password("floppa-local-2026").await?;
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(727101)")
        .execute(&mut *tx)
        .await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(&mut *tx)
        .await?;
    if count > 0 {
        println!("Existing development data preserved; use just dev-reset to start over.");
        return Ok(());
    }
    sqlx::query("UPDATE plans SET price_stars = 150 WHERE name = 'standard'")
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE plans SET price_stars = 300 WHERE name = 'premium'")
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO plan_regions SELECT id, 'singapore' FROM plans WHERE name = 'premium' ON CONFLICT DO NOTHING").execute(&mut *tx).await?;
    let plans: Vec<i32> = sqlx::query_scalar(
        "SELECT id FROM plans WHERE name IN ('basic','standard','premium') ORDER BY id",
    )
    .fetch_all(&mut *tx)
    .await?;
    let encryption_key = secrets.auth.as_ref().unwrap().get_encryption_key()?;
    let names = [
        "Анна Орлова",
        "Александр Соколов",
        "Мария Ким",
        "Дмитрий Волков",
        "Елена Белова",
        "Михаил Лебедев",
    ];
    for i in 0..38_i32 {
        let login = match i {
            0 => "dev-admin".into(),
            1 => "dev-user".into(),
            _ => format!("test-user-{i:02}"),
        };
        let name = match i {
            0 => "Локальный администратор",
            1 => "Тестовый пользователь",
            _ => names[i as usize % names.len()],
        };
        let user_id: i64 = sqlx::query_scalar("INSERT INTO users (username, first_name, is_admin, created_at) VALUES ($1, $2, $3, $4) RETURNING id")
            .bind(&login).bind(name).bind(i == 0).bind(Utc::now() - Duration::days(i64::from(i) * 3)).fetch_one(&mut *tx).await?;
        if i < 2 {
            sqlx::query("INSERT INTO auth_identities (user_id, provider, provider_uid, secret_hash) VALUES ($1, 'password', $2, $3)")
                .bind(user_id).bind(&login).bind(&hash).execute(&mut *tx).await?;
        }
        if i % 7 == 6 {
            continue;
        }
        let expires = if i % 9 == 0 {
            None
        } else {
            Some(
                Utc::now()
                    + Duration::days(if i % 5 == 0 {
                        -3
                    } else {
                        i64::from(i % 20 + 1)
                    }),
            )
        };
        services::grant_subscription(&mut tx, user_id, plans[i as usize % plans.len()], expires)
            .await?;
        let installation_id: i64 = sqlx::query_scalar("INSERT INTO app_installations (user_id, device_id, device_name, platform, app_version) VALUES ($1, $2, $3, $4, '0.6.4') RETURNING id")
            .bind(user_id).bind(format!("dev-device-{i}")).bind(if i % 2 == 0 { "Телефон" } else { "Ноутбук" }).bind(if i % 2 == 0 { "android" } else { "linux" }).fetch_one(&mut *tx).await?;
        let (private, public) = wg_keys::generate_keypair()?;
        let encrypted = crypto::encrypt_private_key(private.as_base64(), &encryption_key)?;
        sqlx::query("INSERT INTO peers (user_id, public_key, private_key_encrypted, assigned_ip, sync_status, installation_id, protocol, region_id) VALUES ($1, $2, $3, $4, 'pending_add', $5, 'wireguard', 'europe')")
            .bind(user_id).bind(public.as_base64()).bind(encrypted).bind(format!("10.240.0.{}", i + 2)).bind(installation_id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    println!("Seeded 38 users, subscriptions, devices and pending configs. No VPN daemon runs.");
    Ok(())
}
