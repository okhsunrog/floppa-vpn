//! Floppa node orchestration: PostgreSQL snapshots, one router, two listeners.
use crate::{
    DeviceGrant, DeviceRegistry, TokenDigest,
    policy::{Factory, Policy},
};
use anyhow::{Context, Result, ensure};
use floppa_core::{DbPool, foldfin::NodeConfig};
use foldfin::{admission::PeerId, packet::PacketDevice, router::Router, server::VpnConfig};
use ipnet::IpNet;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::postgres::PgListener;
use std::{
    collections::{HashMap, HashSet},
    net::{SocketAddr, UdpSocket},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{net::TcpListener, sync::watch, task::JoinSet};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessConfig {
    pub region: String,
    pub listen: SocketAddr,
    pub interface: String,
    pub certificate: PathBuf,
    pub private_key: PathBuf,
    pub public: NodeConfig,
    #[serde(default = "default_sync")]
    pub sync_interval_secs: u64,
    #[serde(default = "default_lease")]
    pub lease_secs: u64,
    #[serde(default = "default_metrics")]
    pub metrics: SocketAddr,
}
fn default_sync() -> u64 {
    30
}
fn default_lease() -> u64 {
    3600
}
fn default_metrics() -> SocketAddr {
    "127.0.0.1:9104".parse().unwrap()
}
impl ProcessConfig {
    pub fn validate(&self) -> Result<()> {
        self.public.validate()?;
        ensure!(self.listen.port() != 0, "listen port must be explicit");
        ensure!(
            !self.region.is_empty() && self.region.len() <= 64,
            "invalid node region"
        );
        ensure!(
            !self.interface.is_empty()
                && self.interface.len() < 16
                && self
                    .interface
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "invalid TUN name"
        );
        ensure!(
            (1..=300).contains(&self.sync_interval_secs)
                && (self.sync_interval_secs * 2..=86400).contains(&self.lease_secs),
            "lease must cover at least two sync intervals and at most one day"
        );
        ensure!(
            self.metrics.ip().is_loopback(),
            "metrics must bind loopback"
        );
        Ok(())
    }
}

#[derive(sqlx::FromRow)]
struct Row {
    id: i64,
    revision: i64,
    slot: i32,
    token_digest: Vec<u8>,
    remaining_ms: Option<i64>,
    speed_limit_mbps: Option<i32>,
    private_routes: Vec<String>,
}
struct Prepared {
    id: i64,
    revision: i64,
    digest: TokenDigest,
    expires: Instant,
    fingerprint: [u8; 32],
    vpn: foldfin::vpn::Config,
    policy: Arc<Policy>,
}
struct Applied {
    fingerprint: [u8; 32],
}

pub struct Node {
    pool: DbPool,
    region: String,
    config: NodeConfig,
    horizon: Duration,
    registry: Arc<DeviceRegistry>,
    router: Router,
    applied: HashMap<i64, Applied>,
}
impl Node {
    pub fn new(
        pool: DbPool,
        region: String,
        config: NodeConfig,
        horizon: Duration,
        device: PacketDevice,
    ) -> Result<Self> {
        config.validate()?;
        ensure!(
            !horizon.is_zero() && horizon <= Duration::from_secs(86400),
            "invalid lease horizon"
        );
        Ok(Self {
            pool,
            region,
            config,
            horizon,
            registry: Arc::new(DeviceRegistry::default()),
            router: Router::start(device),
            applied: HashMap::new(),
        })
    }
    pub fn registry(&self) -> &Arc<DeviceRegistry> {
        &self.registry
    }

    /// Serialized owner only. Read a complete consistent snapshot before mutating access.
    /// A failed read/validation never renews an old lease.
    pub async fn synchronize(&mut self) -> Result<()> {
        let observed = Instant::now();
        let (rows, pools) = tokio::time::timeout(Duration::from_secs(5), async {
            let mut tx = self.pool.begin().await?;
            sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY").execute(&mut *tx).await?;
            let (metadata, enabled): (String, bool) = sqlx::query_as("SELECT config::text, enabled FROM foldfin_nodes WHERE region_id=$1")
                .bind(&self.region).fetch_one(&mut *tx).await?;
            let published: NodeConfig = serde_json::from_str(&metadata)?;
            ensure!(published == self.config, "node metadata differs from prepared network configuration");
            let all: Vec<String> = sqlx::query_scalar("SELECT config::text FROM foldfin_nodes").fetch_all(&mut *tx).await?;
            let mut pools = Vec::new();
            for json in all {
                let config: NodeConfig = serde_json::from_str(&json)?;
                config.validate()?;
                pools.push(config.ipv4_pool.to_string().parse::<IpNet>()?);
                if let Some(pool) = config.ipv6_pool { pools.push(pool.to_string().parse()?); }
            }
            pools.sort();
            pools.dedup();
            let rows = if enabled {
                sqlx::query_as::<_, Row>(r#"
                    SELECT d.id,d.revision,d.slot,d.token_digest,
                      (EXTRACT(EPOCH FROM (cs.expires_at - statement_timestamp())) * 1000)::bigint AS remaining_ms,
                      cs.speed_limit_mbps AS speed_limit_mbps,
                      ARRAY(SELECT DISTINCT unnest(pn.cidrs)::text FROM plan_private_networks pp
                            JOIN private_networks pn ON pn.id=pp.network_id AND pn.is_active
                            WHERE pp.plan_id=cs.plan_id) AS private_routes
                    FROM foldfin_devices d
                    JOIN app_installations ai ON ai.id=d.installation_id AND ai.region_id=d.region_id
                    JOIN sessions s ON s.id=d.issued_by_session AND s.user_id=ai.user_id AND s.revoked_at IS NULL
                    JOIN current_subscriptions cs ON cs.user_id=ai.user_id AND cs.is_active
                    JOIN plan_regions pr ON pr.plan_id=cs.plan_id AND pr.region_id=d.region_id
                    JOIN regions r ON r.id=d.region_id AND r.is_active
                    WHERE d.region_id=$1 AND d.revoked_at IS NULL ORDER BY d.id LIMIT 4097
                "#).bind(&self.region).fetch_all(&mut *tx).await?
            } else { Vec::new() };
            tx.commit().await?;
            Ok::<_, anyhow::Error>((rows,pools))
        }).await.context("grant snapshot timed out")??;
        ensure!(rows.len() <= 4096, "node device capacity exceeded");
        let mut prepared = Vec::with_capacity(rows.len());
        let mut digests = HashSet::new();
        for row in rows {
            ensure!(
                row.slot >= 2 && row.slot <= self.config.last_slot(),
                "device address outside configured pool"
            );
            ensure!(row.revision > 0, "invalid device version");
            let bytes: [u8; 32] = row
                .token_digest
                .try_into()
                .map_err(|_| anyhow::anyhow!("invalid credential digest length"))?;
            ensure!(digests.insert(bytes), "duplicate credential digest");
            let lifetime = row
                .remaining_ms
                .map(|ms| Duration::from_millis(ms.max(0) as u64))
                .unwrap_or(self.horizon)
                .min(self.horizon);
            let expires = observed + lifetime;
            if expires <= Instant::now() {
                continue;
            }
            let speed_mbps = row
                .speed_limit_mbps
                .map(|n| {
                    u32::try_from(n)
                        .ok()
                        .filter(|n| *n > 0)
                        .context("invalid plan speed limit")
                })
                .transpose()?;
            let mut private = self
                .config
                .allow_destinations
                .iter()
                .map(|p| p.to_string().parse::<IpNet>())
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let mut routes = self
                .config
                .routes
                .iter()
                .map(|p| p.to_string().parse::<IpNet>())
                .collect::<std::result::Result<Vec<_>, _>>()?;
            for route in row.private_routes {
                let route: IpNet = route.parse()?;
                if !private.contains(&route) {
                    private.push(route);
                }
                if !routes.contains(&route) {
                    routes.push(route);
                }
            }
            ensure!(private.len() <= 64, "too many private grants");
            ensure!(
                !private.iter().any(|a| pools
                    .iter()
                    .any(|b| a.contains(&b.network()) || b.contains(&a.network()))),
                "private grant overlaps client pool"
            );
            private.sort();
            routes.sort();
            let vpn = foldfin::vpn::Config {
                local: self.config.addresses(1),
                peer: self.config.addresses(row.slot as u32),
                local_routes: routes,
                peer_routes: Vec::new(),
            };
            vpn.validate()?;
            let fingerprint: [u8; 32] = Sha256::digest(serde_json::to_vec(&(
                row.revision,
                bytes,
                row.slot,
                &private,
                &pools,
                speed_mbps,
            ))?)
            .into();
            prepared.push(Prepared {
                id: row.id,
                revision: row.revision,
                digest: TokenDigest::from_bytes(bytes),
                expires,
                fingerprint,
                vpn,
                policy: Arc::new(Policy {
                    pools: pools.clone(),
                    gateways: self.config.addresses(1),
                    private,
                    speed_mbps,
                }),
            });
        }
        let wanted: HashSet<_> = prepared.iter().map(|p| p.id).collect();
        self.applied.retain(|id, _| {
            if wanted.contains(id) {
                true
            } else {
                self.registry.revoke(&id.to_string());
                false
            }
        });
        let mut acknowledged = Vec::new();
        for grant in prepared {
            let identity = grant.id.to_string();
            let unchanged = self
                .applied
                .get(&grant.id)
                .is_some_and(|a| a.fingerprint == grant.fingerprint);
            if !unchanged || self.registry.renew(&identity, grant.expires).is_err() {
                let inner = self
                    .router
                    .peer(PeerId::new(&identity)?, grant.vpn.clone())?;
                let factory = Factory {
                    inner,
                    policy: grant.policy,
                };
                self.registry
                    .install(DeviceGrant {
                        device_id: identity,
                        token_digest: grant.digest,
                        expires_at: grant.expires,
                        proxy: false,
                        allow_private_proxy: false,
                        vpn: Some(VpnConfig {
                            policy: grant.vpn,
                            device: Arc::new(factory),
                        }),
                    })
                    .map_err(anyhow::Error::msg)?;
                self.applied.insert(
                    grant.id,
                    Applied {
                        fingerprint: grant.fingerprint,
                    },
                );
            }
            acknowledged.push((grant.id, grant.revision));
        }
        self.registry.prune_expired();
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut tx = self.pool.begin().await?;
            for (id, version) in acknowledged {
                sqlx::query(
                    "UPDATE foldfin_devices SET applied_revision=$2 WHERE id=$1 AND revision=$2",
                )
                .bind(id)
                .bind(version)
                .execute(&mut *tx)
                .await?;
            }
            sqlx::query("UPDATE foldfin_nodes SET last_sync_at=NOW() WHERE region_id=$1")
                .bind(&self.region)
                .execute(&mut *tx)
                .await?;
            tx.commit().await
        })
        .await
        .context("grant acknowledgement timed out")??;
        metrics::gauge!("floppa_foldfin_configured_grants").set(self.applied.len() as f64);
        metrics::gauge!("floppa_foldfin_control_plane_healthy").set(1.0);
        metrics::counter!("floppa_foldfin_sync_success_total").increment(1);
        Ok(())
    }

    /// Own and join listeners before releasing the shared packet device.
    pub async fn serve(
        mut self,
        tls: Arc<foldfin::server::TlsConfig>,
        tcp: TcpListener,
        udp: UdpSocket,
        interval: Duration,
        shutdown: impl std::future::Future<Output = ()>,
    ) -> Result<()> {
        let (cancel, stopped) = watch::channel(false);
        let mut tasks = JoinSet::new();
        let result = async {
            ensure!(!interval.is_zero() && interval * 2 <= self.horizon, "invalid synchronization interval");
            let mut notifications = tokio::time::timeout(Duration::from_secs(5), async {
                let mut listener = PgListener::connect_with(&self.pool).await?;
                listener.listen_all(["foldfin_changed", "subscription_changed"]).await?;
                Ok::<_, sqlx::Error>(listener)
            }).await.context("notification setup timed out")??;
            self.synchronize().await.context("initial grant synchronization")?;
            let server = self.registry.server(tls)?;
            let h2 = server.clone();
            let mut stop_h2 = stopped.clone();
            tasks.spawn(async move { h2.serve(tcp, async { let _ = stop_h2.wait_for(|v| *v).await; }).await });
            let mut stop_h3 = stopped;
            tasks.spawn(async move { server.serve_h3(udp, async { let _ = stop_h3.wait_for(|v| *v).await; }).await });
            let mut tick = tokio::time::interval(interval);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            tick.tick().await;
            tokio::pin!(shutdown);
            loop {
                tokio::select! {
                    biased;
                    _ = &mut shutdown => break Ok(()),
                    _ = self.router.closed() => break Err(anyhow::anyhow!("shared packet device stopped")),
                    task = tasks.join_next() => break Err(anyhow::anyhow!("Foldfin listener exited: {task:?}")),
                    _ = tick.tick() => (),
                    notification = notifications.try_recv() => {
                        if notification.is_err() {
                            // Periodic reconciliation remains authoritative; avoid a hot error loop.
                            tokio::time::sleep(Duration::from_secs(1)).await;
                        }
                    }
                }
                if let Err(error) = self.synchronize().await {
                    metrics::counter!("floppa_foldfin_sync_failure_total").increment(1);
                    metrics::gauge!("floppa_foldfin_control_plane_healthy").set(0.0);
                    tracing::warn!(%error, "Foldfin grant synchronization failed; existing leases keep their deadlines");
                }
                self.registry.prune_expired();
                let stats = self.router.stats();
                metrics::gauge!("floppa_foldfin_router_queue_drops").set(stats.full_queue_drops as f64);
                metrics::gauge!("floppa_foldfin_router_unroutable_packets").set(stats.unroutable_packets as f64);
            }
        }.await;
        cancel.send_replace(true);
        let mut listener_error = None;
        while let Some(joined) = tasks.join_next().await {
            match joined {
                Ok(Ok(())) => (),
                Ok(Err(error)) => {
                    listener_error.get_or_insert(anyhow::Error::new(error));
                }
                Err(error) => {
                    listener_error.get_or_insert(anyhow::Error::new(error));
                }
            }
        }
        let router = self.router.stop().await;
        result?;
        if let Some(error) = listener_error {
            return Err(error);
        }
        router?;
        Ok(())
    }
}
