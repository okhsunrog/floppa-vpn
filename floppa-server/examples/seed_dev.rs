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
        // Deterministic purchase histories for the real analytics API. These are local
        // fixtures, not synthetic responses in the frontend. Keep manual grants distinct.
        let source = match i % 6 {
            0 => "admin_grant",
            1 => "trial",
            2 => "taster",
            _ => "purchase",
        };
        let at = Utc::now() - Duration::days(i64::from(i % 28));
        let plan = if source == "purchase" {
            plans[1 + (i as usize % 2)]
        } else {
            plans[i as usize % plans.len()]
        };
        let subscription: i64 = sqlx::query_scalar("UPDATE subscriptions SET source=$2, starts_at=$3, plan_id=$4 WHERE user_id=$1 AND is_current RETURNING id")
            .bind(user_id).bind(source).bind(at).bind(plan).fetch_one(&mut *tx).await?;
        if source == "purchase" {
            if i % 2 == 0 {
                let old_at = at - Duration::days(35);
                let old_sub: i64 = sqlx::query_scalar("INSERT INTO subscriptions (user_id,plan_id,source,starts_at,expires_at,is_current) VALUES ($1,$2,'purchase',$3,$4,false) RETURNING id")
                    .bind(user_id).bind(plan).bind(old_at).bind(at).fetch_one(&mut *tx).await?;
                sqlx::query("INSERT INTO payments (user_id,plan_id,subscription_id,amount,invoice_payload,status,completed_at) VALUES ($1,$2,$3,150,'dev-first','completed',$4)")
                    .bind(user_id).bind(plan).bind(old_sub).bind(old_at).execute(&mut *tx).await?;
            }
            sqlx::query("INSERT INTO payments (user_id,plan_id,subscription_id,amount,invoice_payload,status,completed_at) VALUES ($1,$2,$3,$4,'dev-paid','completed',$5)")
                .bind(user_id).bind(plan).bind(subscription).bind(if i % 2 == 0 {150_i32} else {300_i32}).bind(at).execute(&mut *tx).await?;
            if i % 5 == 0 {
                sqlx::query("INSERT INTO payments (user_id,plan_id,subscription_id,amount,credit_amount,invoice_payload,status,completed_at) VALUES ($1,$2,$3,0,150,'dev-credit','completed',$4)")
                    .bind(user_id).bind(plan).bind(subscription).bind(at + Duration::hours(1)).execute(&mut *tx).await?;
            }
        }
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
