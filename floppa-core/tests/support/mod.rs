use floppa_core::{
    DbPool,
    foldfin::{self, NodeConfig},
};

pub fn config() -> NodeConfig {
    serde_json::from_value(serde_json::json!({
        "endpoints":["localhost:443"], "server_name":"localhost",
        "ipv4_pool":"10.33.0.0/24", "ipv6_pool":"fd33::/120",
        "routes":["0.0.0.0/0","::/0"], "dns":["1.1.1.1"]
    }))
    .unwrap()
}
pub async fn seed(pool: &DbPool, max: i32) -> (i64, i32) {
    let user = sqlx::query_scalar("INSERT INTO users DEFAULT VALUES RETURNING id")
        .fetch_one(pool)
        .await
        .unwrap();
    let plan = sqlx::query_scalar("INSERT INTO plans(name,display_name,max_peers) VALUES('foldfin-test','Test',$1) RETURNING id").bind(max).fetch_one(pool).await.unwrap();
    sqlx::query(
        "INSERT INTO subscriptions(user_id,plan_id,starts_at,is_current) VALUES($1,$2,NOW(),true)",
    )
    .bind(user)
    .bind(plan)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO plan_regions(plan_id,region_id) VALUES($1,'singapore')")
        .bind(plan)
        .execute(pool)
        .await
        .unwrap();
    foldfin::register_node(pool, "singapore", &config())
        .await
        .unwrap();
    (user, plan)
}
pub async fn device(pool: &DbPool, user: i64, name: &str) -> (i64, String) {
    let installation = sqlx::query_scalar("INSERT INTO app_installations(user_id,device_id,region_id) VALUES($1,$2,'singapore') RETURNING id").bind(user).bind(name).fetch_one(pool).await.unwrap();
    let session = sqlx::query_scalar("INSERT INTO sessions(id,user_id,installation_id,kind) VALUES(gen_random_uuid(),$1,$2,'credential') RETURNING id::text").bind(user).bind(installation).fetch_one(pool).await.unwrap();
    (installation, session)
}
