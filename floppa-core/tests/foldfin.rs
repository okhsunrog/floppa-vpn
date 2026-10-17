mod support;
use floppa_core::{
    DbPool, FloppaError,
    foldfin::{self, Error},
};
use support::*;

#[sqlx::test(migrations = "../migrations")]
async fn rotation_ownership_logout_and_region_change(pool: DbPool) {
    let (user, _) = seed(&pool, 2).await;
    let (installation, session) = device(&pool, user, "first").await;
    let session = session.parse().unwrap();
    assert!(matches!(
        foldfin::issue(&pool, user + 1, session, false).await,
        Err(Error::Policy(FloppaError::NoActiveSubscription))
    ));
    let issued = foldfin::issue(&pool, user, session, false).await.unwrap();
    assert!(matches!(
        foldfin::issue(&pool, user, session, false).await,
        Err(Error::Exists)
    ));
    let refreshed =
        foldfin::issue_with_token(&pool, user, session, false, issued.config["token"].as_str())
            .await
            .unwrap();
    assert_eq!(refreshed.config, issued.config);
    assert_eq!(refreshed.revision, issued.revision);
    assert!(matches!(
        foldfin::issue_with_token(&pool, user, session, false, Some("wrong")).await,
        Err(Error::Exists)
    ));
    let rotated = foldfin::issue(&pool, user, session, true).await.unwrap();
    assert_eq!(issued.device_id, rotated.device_id);
    assert_eq!(
        issued.config["network"]["addresses"],
        rotated.config["network"]["addresses"]
    );
    assert_ne!(issued.config["token"], rotated.config["token"]);
    assert_eq!(rotated.revision, issued.revision + 1);
    let stored: Vec<u8> =
        sqlx::query_scalar("SELECT token_digest FROM foldfin_devices WHERE id=$1")
            .bind(issued.device_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    use sha2::{Digest, Sha256};
    assert_eq!(
        stored,
        Sha256::digest(rotated.config["token"].as_str().unwrap().as_bytes()).to_vec()
    );
    assert!(
        !foldfin::revoke(&pool, user + 1, issued.device_id)
            .await
            .unwrap()
    );
    assert!(foldfin::list(&pool, user + 1).await.unwrap().is_empty());
    sqlx::query("UPDATE sessions SET revoked_at=NOW() WHERE id=$1")
        .bind(session)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        foldfin::list(&pool, user).await.unwrap()[0]
            .revoked_at
            .is_some()
    );
    assert!(matches!(
        foldfin::issue(&pool, user, session, true).await,
        Err(Error::Session)
    ));
    let session: uuid::Uuid = sqlx::query_scalar("INSERT INTO sessions(id,user_id,installation_id,kind) VALUES(gen_random_uuid(),$1,$2,'credential') RETURNING id").bind(user).bind(installation).fetch_one(&pool).await.unwrap();
    foldfin::issue(&pool, user, session, false).await.unwrap();
    sqlx::query("UPDATE app_installations SET region_id='europe' WHERE id=$1")
        .bind(installation)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        foldfin::list(&pool, user).await.unwrap()[0]
            .revoked_at
            .is_some()
    );
}

#[sqlx::test(migrations = "../migrations")]
async fn concurrent_devices_obey_quota_and_revocation_releases_slot(pool: DbPool) {
    let (user, _) = seed(&pool, 1).await;
    let (_, a) = device(&pool, user, "a").await;
    let (_, b) = device(&pool, user, "b").await;
    let (a, b) = (a.parse().unwrap(), b.parse().unwrap());
    let (first, second) = tokio::join!(
        foldfin::issue(&pool, user, a, false),
        foldfin::issue(&pool, user, b, false)
    );
    let (issued, denied, next) = match (first, second) {
        (Ok(v), Err(e)) => (v, e, b),
        (Err(e), Ok(v)) => (v, e, a),
        _ => panic!("quota was not serialized"),
    };
    assert!(matches!(
        denied,
        Error::Policy(FloppaError::PeerLimitReached { current: 1, max: 1 })
    ));
    assert!(
        foldfin::revoke(&pool, user, issued.device_id)
            .await
            .unwrap()
    );
    foldfin::issue(&pool, user, next, false).await.unwrap();
}

#[sqlx::test(migrations = "../migrations")]
async fn wireguard_and_foldfin_share_installation_slot(pool: DbPool) {
    let (user, _) = seed(&pool, 1).await;
    let (installation, session) = device(&pool, user, "shared").await;
    sqlx::query("INSERT INTO peers(user_id,installation_id,public_key,assigned_ip) VALUES($1,$2,'test-key','10.200.0.2')").bind(user).bind(installation).execute(&pool).await.unwrap();
    foldfin::issue(&pool, user, session.parse().unwrap(), false)
        .await
        .unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM vpn_device_slots WHERE user_id=$1")
        .bind(user)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    let (_, second) = device(&pool, user, "other").await;
    assert!(matches!(
        foldfin::issue(&pool, user, second.parse().unwrap(), false).await,
        Err(Error::Policy(FloppaError::PeerLimitReached { .. }))
    ));
}

#[sqlx::test(migrations = "../migrations")]
async fn node_metadata_cannot_overlap_or_change_allocated_addresses(pool: DbPool) {
    let (user, _) = seed(&pool, 1).await;
    assert!(
        foldfin::register_node(&pool, "europe", &config())
            .await
            .is_err()
    );
    let (_, session) = device(&pool, user, "a").await;
    foldfin::issue(&pool, user, session.parse().unwrap(), false)
        .await
        .unwrap();
    let mut changed = config();
    changed.ipv4_pool = "10.34.0.0/24".parse().unwrap();
    assert!(
        foldfin::register_node(&pool, "singapore", &changed)
            .await
            .is_err()
    );
    foldfin::register_node(&pool, "singapore", &config())
        .await
        .unwrap();
}

#[sqlx::test(migrations = "../migrations")]
async fn policy_changes_publish_revision_but_expiry_extension_does_not(pool: DbPool) {
    let (user, plan) = seed(&pool, 1).await;
    let (_, session) = device(&pool, user, "a").await;
    let issued = foldfin::issue(&pool, user, session.parse().unwrap(), false)
        .await
        .unwrap();
    sqlx::query("UPDATE subscriptions SET expires_at=NOW()+INTERVAL '1 day' WHERE user_id=$1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        foldfin::list(&pool, user).await.unwrap()[0].revision,
        issued.revision
    );
    sqlx::query("UPDATE plans SET default_speed_limit_mbps=10 WHERE id=$1")
        .bind(plan)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        foldfin::list(&pool, user).await.unwrap()[0].revision,
        issued.revision + 1
    );
    sqlx::query("UPDATE users SET is_admin=true WHERE id=$1")
        .bind(user)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE plans SET is_public=false WHERE id=$1")
        .bind(plan)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO private_networks(id,display_name,cidrs) VALUES('foldfin-test','Test',ARRAY['10.44.0.0/24'::cidr])").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO plan_private_networks(plan_id,network_id) VALUES($1,'foldfin-test')")
        .bind(plan)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        foldfin::list(&pool, user).await.unwrap()[0].revision,
        issued.revision + 2
    );
    sqlx::query(
        "UPDATE private_networks SET cidrs=ARRAY['10.45.0.0/24'::cidr] WHERE id='foldfin-test'",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        foldfin::list(&pool, user).await.unwrap()[0].revision,
        issued.revision + 3
    );
    let rotated = foldfin::issue(&pool, user, session.parse().unwrap(), true)
        .await
        .unwrap();
    assert!(
        rotated.config["network"]["routes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "10.45.0.0/24")
    );
    let other: i32 = sqlx::query_scalar(
        "INSERT INTO plans(name,display_name) VALUES('other-plan','Other') RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let before = foldfin::list(&pool, user).await.unwrap()[0].revision;
    let mut tx = pool.begin().await.unwrap();
    floppa_core::services::replace_active_subscription(
        &mut tx,
        user,
        other,
        None,
        floppa_core::SubscriptionSource::AdminGrant,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        foldfin::list(&pool, user).await.unwrap()[0].revision,
        before + 1
    );
}

#[sqlx::test(migrations = "../migrations")]
async fn plan_access_defaults_closed_and_revokes_existing_credentials(pool: DbPool) {
    let (user, plan) = seed(&pool, 2).await;
    let (_, session) = device(&pool, user, "gated").await;
    let session = session.parse().unwrap();
    let default_enabled: bool = sqlx::query_scalar("INSERT INTO plans(name,display_name) VALUES('ordinary','Ordinary') RETURNING foldfin_enabled")
        .fetch_one(&pool).await.unwrap();
    assert!(!default_enabled);
    let issued = foldfin::issue(&pool, user, session, false).await.unwrap();
    sqlx::query("UPDATE plans SET foldfin_enabled=false WHERE id=$1")
        .bind(plan)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        foldfin::list(&pool, user).await.unwrap()[0]
            .revoked_at
            .is_some()
    );
    assert!(matches!(
        foldfin::issue_with_token(&pool, user, session, false, issued.config["token"].as_str())
            .await,
        Err(Error::Unavailable)
    ));
    assert!(matches!(
        foldfin::issue(&pool, user, session, true).await,
        Err(Error::Unavailable)
    ));
    sqlx::query("UPDATE plans SET foldfin_enabled=true WHERE id=$1")
        .bind(plan)
        .execute(&pool)
        .await
        .unwrap();
    let fresh = foldfin::issue(&pool, user, session, false).await.unwrap();
    assert_ne!(fresh.config["token"], issued.config["token"]);
}
