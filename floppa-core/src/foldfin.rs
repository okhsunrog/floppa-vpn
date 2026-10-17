//! Foldfin control-plane records. No transport engine or packet processing here.
use crate::{DbPool, FloppaError};
use chrono::{DateTime, Utc};
use ipnetwork::{IpNetwork, Ipv4Network, Ipv6Network};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Policy(#[from] FloppaError),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error("invalid Foldfin configuration: {0}")]
    Config(String),
    #[error("Foldfin is not available in this region")]
    Unavailable,
    #[error("Foldfin configuration already exists; explicit rotation is required")]
    Exists,
    #[error("a live device-bound API session is required")]
    Session,
}
type Result<T> = std::result::Result<T, Error>;

/// Public node metadata: deployment publishes it, issuance and serving share it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeConfig {
    pub endpoints: Vec<String>,
    pub server_name: String,
    pub ipv4_pool: Ipv4Network,
    pub ipv6_pool: Option<Ipv6Network>,
    pub routes: Vec<IpNetwork>,
    pub dns: Vec<IpAddr>,
    #[serde(default)]
    pub allow_destinations: Vec<IpNetwork>,
    #[serde(default)]
    pub additional_ca_pem: Option<String>,
}
impl NodeConfig {
    pub fn validate(&self) -> Result<()> {
        let invalid = |message: &str| Error::Config(message.into());
        if self.endpoints.is_empty()
            || self.endpoints.len() > 8
            || self
                .endpoints
                .iter()
                .any(|e| e.parse::<floppa_tunnel_config::Endpoint>().is_err())
        {
            return Err(invalid("expected 1..8 valid endpoints"));
        }
        if self.server_name.is_empty()
            || self.server_name.len() > 253
            || self
                .server_name
                .chars()
                .any(|c| c.is_whitespace() || c.is_control())
            || self.server_name.contains('/')
        {
            return Err(invalid("invalid TLS server name"));
        }
        if !(16..=30).contains(&self.ipv4_pool.prefix())
            || self.ipv4_pool.ip() != self.ipv4_pool.network()
            || !self.ipv4_pool.network().is_private()
        {
            return Err(invalid("IPv4 pool must be a canonical private /16../30"));
        }
        if let Some(pool) = self.ipv6_pool
            && (!(112..=126).contains(&pool.prefix())
                || pool.ip() != pool.network()
                || !pool.network().is_unique_local())
        {
            return Err(invalid("IPv6 pool must be a canonical ULA /112../126"));
        }
        if self.routes.is_empty()
            || self.routes.len() > 48
            || self.allow_destinations.len() > 32
            || self
                .routes
                .iter()
                .chain(&self.allow_destinations)
                .any(|n| n.ip() != n.network() || (n.is_ipv6() && self.ipv6_pool.is_none()))
        {
            return Err(invalid("invalid routed prefixes"));
        }
        if self
            .allow_destinations
            .iter()
            .any(|n| self.overlaps_pool(*n))
        {
            return Err(invalid(
                "private destination permission overlaps the client pool",
            ));
        }
        if self.dns.is_empty()
            || self.dns.len() > 8
            || self.dns.iter().any(|ip| {
                ip.is_unspecified()
                    || ip.is_multicast()
                    || ip.is_loopback()
                    || !self.routes.iter().any(|n| n.contains(*ip))
                    || (!public_destination(*ip)
                        && !self.allow_destinations.iter().any(|n| n.contains(*ip)))
            })
        {
            return Err(invalid("DNS must be reachable through provisioned routes"));
        }
        Ok(())
    }
    pub fn overlaps_pool(&self, network: IpNetwork) -> bool {
        let pools =
            std::iter::once(IpNetwork::V4(self.ipv4_pool)).chain(self.ipv6_pool.map(IpNetwork::V6));
        pools
            .into_iter()
            .any(|p| p.contains(network.network()) || network.contains(p.network()))
    }
    pub fn last_slot(&self) -> i32 {
        let ipv4 = self.ipv4_pool.size() - 2;
        let ipv6 = self
            .ipv6_pool
            .map_or(65534, |p| ((1_u128 << (128 - p.prefix())) - 1) as u32);
        ipv4.min(ipv6).min(65534) as i32
    }
    pub fn addresses(&self, slot: u32) -> Vec<IpAddr> {
        let mut result = vec![IpAddr::V4(Ipv4Addr::from(
            u32::from(self.ipv4_pool.network()) + slot,
        ))];
        if let Some(pool) = self.ipv6_pool {
            result.push(IpAddr::V6(Ipv6Addr::from(
                u128::from(pool.network()) + u128::from(slot),
            )));
        }
        result
    }
    pub fn document(
        &self,
        slot: u32,
        revision: i64,
        token: &str,
        private: &[IpNetwork],
    ) -> serde_json::Value {
        let mut routes = self.routes.clone();
        for route in private {
            if !routes.contains(route) {
                routes.push(*route);
            }
        }
        serde_json::json!({
            "version": 2, "endpoints": self.endpoints, "server_name": self.server_name,
            "token": token, "transport": "auto", "additional_ca_pem": self.additional_ca_pem,
            "network": { "revision": revision, "addresses": self.addresses(slot).into_iter().map(|ip| {
                format!("{ip}/{}", if ip.is_ipv4() { 32 } else { 128 })
            }).collect::<Vec<_>>(), "peers": self.addresses(1), "routes": routes,
                "excluded_routes": [], "dns": self.dns, "mtu": 1280 }
        })
    }
}

/// Conservative internet destination policy shared by issuance and node packet IO.
pub fn public_destination(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let [a, b, _, _] = v.octets();
            !(v.is_private()
                || v.is_loopback()
                || v.is_link_local()
                || v.is_documentation()
                || a == 0
                || a >= 224
                || (a == 100 && (64..128).contains(&b))
                || (a == 192 && b == 0)
                || (a == 198 && (b == 18 || b == 19)))
        }
        IpAddr::V6(v) => {
            let s = v.segments();
            s[0] & 0xe000 == 0x2000 && !(s[0] == 0x2001 && s[1] == 0x0db8)
        }
    }
}

/// Deployment registration is explicit; ordinary node restarts never rewrite metadata.
pub async fn register_node(pool: &DbPool, region: &str, config: &NodeConfig) -> Result<()> {
    config.validate()?;
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('foldfin-pools', 0))")
        .execute(&mut *tx)
        .await?;
    let existing = sqlx::query_as::<_, (String, String)>(
        "SELECT region_id, config::text FROM foldfin_nodes FOR UPDATE",
    )
    .fetch_all(&mut *tx)
    .await?;
    for (other, json) in existing {
        let old: NodeConfig = serde_json::from_str(&json)
            .map_err(|_| Error::Config("invalid stored node metadata".into()))?;
        if other != region
            && (old.overlaps_pool(IpNetwork::V4(config.ipv4_pool))
                || config
                    .ipv6_pool
                    .is_some_and(|p| old.overlaps_pool(IpNetwork::V6(p))))
        {
            return Err(Error::Config("client pools overlap another node".into()));
        }
        if other == region && old != *config {
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM foldfin_devices WHERE region_id = $1)",
            )
            .bind(region)
            .fetch_one(&mut *tx)
            .await?;
            if exists {
                return Err(Error::Config("node metadata is immutable while device allocations exist; drain and reprovision explicitly".into()));
            }
        }
    }
    sqlx::query("INSERT INTO foldfin_nodes(region_id, config) VALUES ($1, $2::jsonb) ON CONFLICT(region_id) DO UPDATE SET config = EXCLUDED.config, updated_at = NOW() WHERE foldfin_nodes.config IS DISTINCT FROM EXCLUDED.config")
        .bind(region).bind(serde_json::to_string(config).expect("serializable node config")).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

pub struct Issued {
    pub device_id: i64,
    pub revision: i64,
    /// Contains the one-time secret. Do not derive Debug or record in logs.
    pub config: serde_json::Value,
}

pub async fn issue(pool: &DbPool, user: i64, session: Uuid, rotate: bool) -> Result<Issued> {
    issue_with_token(pool, user, session, rotate, None).await
}

/// Refresh metadata without replacing a credential already held by this installation.
/// Only its digest is stored; the supplied token is never logged.
pub async fn issue_with_token(
    pool: &DbPool,
    user: i64,
    session: Uuid,
    rotate: bool,
    token: Option<&str>,
) -> Result<Issued> {
    let mut tx = pool.begin().await?;
    // Match WG's lock order and device quota serialization.
    let max: i32 = sqlx::query_scalar("SELECT cs.max_peers FROM current_subscriptions cs WHERE cs.id = (SELECT id FROM subscriptions WHERE user_id = $1 AND is_current FOR UPDATE) AND cs.is_active")
        .bind(user).fetch_optional(&mut *tx).await?.ok_or(FloppaError::NoActiveSubscription)?;
    let installation: i64 = sqlx::query_scalar::<_, Option<i64>>("SELECT installation_id FROM sessions WHERE id=$1 AND user_id=$2 AND revoked_at IS NULL FOR UPDATE")
        .bind(session).bind(user).fetch_optional(&mut *tx).await?.flatten().ok_or(Error::Session)?;
    let region: String = sqlx::query_scalar(
        "SELECT region_id FROM app_installations WHERE id=$1 AND user_id=$2 FOR UPDATE",
    )
    .bind(installation)
    .bind(user)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(Error::Session)?;
    let entitled: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM current_subscriptions cs JOIN plan_regions pr ON pr.plan_id=cs.plan_id JOIN regions r ON r.id=pr.region_id WHERE cs.user_id=$1 AND cs.is_active AND r.is_active AND r.id=$2)")
        .bind(user).bind(&region).fetch_one(&mut *tx).await?;
    if !entitled {
        return Err(FloppaError::RegionNotAvailable(region).into());
    }
    let metadata: String = sqlx::query_scalar(
        "SELECT config::text FROM foldfin_nodes WHERE region_id=$1 AND enabled FOR UPDATE",
    )
    .bind(&region)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(Error::Unavailable)?;
    let node: NodeConfig = serde_json::from_str(&metadata)
        .map_err(|_| Error::Config("invalid node metadata".into()))?;
    node.validate()?;
    let previous = sqlx::query_as::<_, (i64, i32, bool)>("SELECT id, slot, revoked_at IS NULL FROM foldfin_devices WHERE installation_id=$1 AND region_id=$2 FOR UPDATE")
        .bind(installation).bind(&region).fetch_optional(&mut *tx).await?;
    let reusable = if let (Some((id, _, true)), Some(token)) = (previous, token) {
        sqlx::query_scalar::<_, bool>(
            "SELECT token_digest=$2 AND issued_by_session=$3 FROM foldfin_devices WHERE id=$1",
        )
        .bind(id)
        .bind(Sha256::digest(token.as_bytes()).to_vec())
        .bind(session)
        .fetch_one(&mut *tx)
        .await?
    } else {
        false
    };
    if previous.is_some_and(|(_, _, active)| active) && !rotate && !reusable {
        return Err(Error::Exists);
    }
    let occupied: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM vpn_device_slots WHERE user_id=$1 AND installation_id=$2)",
    )
    .bind(user)
    .bind(installation)
    .fetch_one(&mut *tx)
    .await?;
    if !occupied {
        let count: i64 = sqlx::query_scalar("SELECT (SELECT COUNT(*) FROM vpn_device_slots WHERE user_id=$1) + (SELECT COUNT(*) FROM peers WHERE user_id=$1 AND installation_id IS NULL AND sync_status <> 'removed')")
            .bind(user).fetch_one(&mut *tx).await?;
        if count >= i64::from(max) {
            return Err(FloppaError::PeerLimitReached {
                current: count as i32,
                max,
            }
            .into());
        }
    }
    let active_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM foldfin_devices WHERE region_id=$1 AND revoked_at IS NULL",
    )
    .bind(&region)
    .fetch_one(&mut *tx)
    .await?;
    if !previous.is_some_and(|(_, _, active)| active) && active_count >= 4096 {
        return Err(Error::Unavailable);
    }
    let slot = match previous {
        Some((_, slot, _)) => slot,
        None => sqlx::query_scalar::<_, i32>("SELECT n FROM generate_series(2, $2) n WHERE NOT EXISTS(SELECT 1 FROM foldfin_devices WHERE region_id=$1 AND slot=n) ORDER BY n LIMIT 1")
            .bind(&region).bind(node.last_slot()).fetch_optional(&mut *tx).await?.ok_or(FloppaError::NoAvailableIps)?,
    };
    let private: Vec<String> = sqlx::query_scalar("SELECT DISTINCT unnest(pn.cidrs)::text FROM current_subscriptions cs JOIN plan_private_networks p ON p.plan_id=cs.plan_id JOIN private_networks pn ON pn.id=p.network_id AND pn.is_active WHERE cs.user_id=$1 AND cs.is_active")
        .bind(user).fetch_all(&mut *tx).await?;
    let private = private
        .into_iter()
        .map(|s| {
            s.parse::<IpNetwork>()
                .map_err(|_| Error::Config("invalid private route".into()))
        })
        .collect::<Result<Vec<_>>>()?;
    let all_metadata: Vec<String> = sqlx::query_scalar("SELECT config::text FROM foldfin_nodes")
        .fetch_all(&mut *tx)
        .await?;
    let all_nodes = all_metadata
        .iter()
        .map(|s| {
            serde_json::from_str::<NodeConfig>(s)
                .map_err(|_| Error::Config("invalid stored node metadata".into()))
        })
        .collect::<Result<Vec<_>>>()?;
    if private.len() + node.routes.len() > 60
        || private
            .iter()
            .any(|p| p.is_ipv6() && node.ipv6_pool.is_none())
        || private
            .iter()
            .chain(&node.allow_destinations)
            .any(|p| all_nodes.iter().any(|n| n.overlaps_pool(*p)))
    {
        return Err(Error::Config(
            "private routes exceed policy bounds or overlap a client pool".into(),
        ));
    }
    if reusable && !rotate {
        let (id, slot, _) = previous.expect("validated existing allocation");
        let revision: i64 = sqlx::query_scalar("SELECT revision FROM foldfin_devices WHERE id=$1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        let config = node.document(
            slot as u32,
            revision,
            token.expect("validated token"),
            &private,
        );
        tx.commit().await?;
        return Ok(Issued {
            device_id: id,
            revision,
            config,
        });
    }
    let token = hex::encode(rand::random::<[u8; 32]>());
    let digest = Sha256::digest(token.as_bytes()).to_vec();
    let (id, revision) = sqlx::query_as::<_, (i64, i64)>("INSERT INTO foldfin_devices(installation_id,region_id,slot,token_digest,issued_by_session) VALUES($1,$2,$3,$4,$5) ON CONFLICT(installation_id,region_id) DO UPDATE SET token_digest=EXCLUDED.token_digest, issued_by_session=EXCLUDED.issued_by_session, revoked_at=NULL, revision=foldfin_devices.revision+1 RETURNING id,revision")
        .bind(installation).bind(&region).bind(slot).bind(digest).bind(session).fetch_one(&mut *tx).await?;
    let config = node.document(slot as u32, revision, &token, &private);
    tx.commit().await?;
    Ok(Issued {
        device_id: id,
        revision,
        config,
    })
}

pub async fn revoke(pool: &DbPool, user: i64, device: i64) -> Result<bool> {
    Ok(sqlx::query("UPDATE foldfin_devices d SET revoked_at=COALESCE(d.revoked_at,NOW()), revision=d.revision + CASE WHEN d.revoked_at IS NULL THEN 1 ELSE 0 END FROM app_installations ai WHERE d.id=$1 AND ai.id=d.installation_id AND ai.user_id=$2")
        .bind(device).bind(user).execute(pool).await?.rows_affected() == 1)
}

#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct DeviceStatus {
    pub id: i64,
    pub installation_id: i64,
    pub region_id: String,
    pub revision: i64,
    pub applied_revision: i64,
    pub revoked_at: Option<DateTime<Utc>>,
    pub node_last_sync_at: Option<DateTime<Utc>>,
}
pub async fn list(pool: &DbPool, user: i64) -> Result<Vec<DeviceStatus>> {
    Ok(sqlx::query_as("SELECT d.id,d.installation_id,d.region_id,d.revision,d.applied_revision,d.revoked_at,n.last_sync_at AS node_last_sync_at FROM foldfin_devices d JOIN app_installations ai ON ai.id=d.installation_id JOIN foldfin_nodes n ON n.region_id=d.region_id WHERE ai.user_id=$1 ORDER BY d.id")
        .bind(user).fetch_all(pool).await?)
}
