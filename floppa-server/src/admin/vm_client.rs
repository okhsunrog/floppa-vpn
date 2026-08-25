//! VictoriaMetrics query client for reading traffic metrics.
//!
//! All figures are from the **client's** perspective: the server's TX counter is what the
//! client downloaded, the server's RX counter is what it uploaded.

use std::collections::HashMap;

use anyhow::{Context, Result};
use serde::Deserialize;
use tracing::warn;

/// Which tunnel's counters to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrafficMetric {
    /// WireGuard + AmneziaWG peers (`wg_*_bytes_total`, labelled by `peer_id` and `user_id`).
    Wg,
    /// VLESS (`vless_*_bytes_total`, labelled by `user_id`).
    Vless,
}

impl TrafficMetric {
    /// Server-side transmit counter = client download.
    fn tx(self) -> &'static str {
        match self {
            TrafficMetric::Wg => "wg_tx_bytes_total",
            TrafficMetric::Vless => "vless_tx_bytes_total",
        }
    }

    /// Server-side receive counter = client upload.
    fn rx(self) -> &'static str {
        match self {
            TrafficMetric::Wg => "wg_rx_bytes_total",
            TrafficMetric::Vless => "vless_rx_bytes_total",
        }
    }
}

/// Bytes moved over a window, from the client's point of view.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Traffic {
    pub download: i64,
    pub upload: i64,
}

/// Traffic is decorative: a failed VictoriaMetrics query must never fail the request it
/// decorates, but it must not vanish silently either. Logs at warn and yields `None`, so the
/// caller can both fall back to zeros and tell the client the figures are missing.
pub fn logged<T>(what: &'static str, result: Result<T>) -> Option<T> {
    result
        .inspect_err(|e| {
            crate::metrics::vm_query_failed(what);
            warn!(
                query = what,
                error = format!("{e:#}"),
                "VictoriaMetrics query failed"
            )
        })
        .ok()
}

#[derive(Deserialize)]
struct VmResponse {
    data: VmData,
}

#[derive(Deserialize)]
struct VmData {
    result: Vec<VmResult>,
}

#[derive(Deserialize)]
struct VmResult {
    metric: HashMap<String, String>,
    value: (f64, String), // (timestamp, value_string)
}

/// A MetricsQL client bound to one VictoriaMetrics instance.
#[derive(Clone)]
pub struct VmClient {
    http: reqwest::Client,
    base_url: String,
}

impl VmClient {
    pub fn new(http: reqwest::Client, base_url: String) -> Self {
        Self { http, base_url }
    }

    /// Execute a MetricsQL instant query.
    async fn query(&self, promql: &str) -> Result<Vec<(HashMap<String, String>, f64)>> {
        let resp: VmResponse = self
            .http
            .get(format!("{}/api/v1/query", self.base_url))
            .query(&[("query", promql)])
            .send()
            .await
            .context("VM query failed")?
            .error_for_status()
            .context("VM returned error status")?
            .json()
            .await
            .context("Failed to parse VM response")?;

        Ok(resp
            .data
            .result
            .into_iter()
            .map(|r| {
                let value: f64 = r.value.1.parse().unwrap_or(0.0);
                (r.metric, value)
            })
            .collect())
    }

    /// Run the download and upload queries side by side and pair the results by `label`.
    async fn traffic_by_label(
        &self,
        label: &str,
        download_query: &str,
        upload_query: &str,
    ) -> Result<HashMap<i64, Traffic>> {
        let (downloads, uploads) =
            tokio::try_join!(self.query(download_query), self.query(upload_query))?;

        let mut result: HashMap<i64, Traffic> = HashMap::new();
        for (labels, value) in &downloads {
            if let Some(id) = labels.get(label).and_then(|s| s.parse::<i64>().ok()) {
                result.entry(id).or_default().download = *value as i64;
            }
        }
        for (labels, value) in &uploads {
            if let Some(id) = labels.get(label).and_then(|s| s.parse::<i64>().ok()) {
                result.entry(id).or_default().upload = *value as i64;
            }
        }
        Ok(result)
    }

    /// Run two scalar queries side by side; a missing series counts as zero.
    async fn traffic_scalar(&self, download_query: &str, upload_query: &str) -> Result<Traffic> {
        let (downloads, uploads) =
            tokio::try_join!(self.query(download_query), self.query(upload_query))?;
        let first = |rows: Vec<(HashMap<String, String>, f64)>| {
            rows.first().map(|(_, v)| *v as i64).unwrap_or(0)
        };
        Ok(Traffic {
            download: first(downloads),
            upload: first(uploads),
        })
    }

    /// Per-peer WG traffic for the given peer IDs over the last `days`.
    pub async fn peer_traffic(&self, peer_ids: &[i64], days: u32) -> Result<HashMap<i64, Traffic>> {
        if peer_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let ids_regex = peer_ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join("|");
        let m = TrafficMetric::Wg;
        let download = format!(r#"increase({}{{peer_id=~"{ids_regex}"}}[{days}d])"#, m.tx());
        let upload = format!(r#"increase({}{{peer_id=~"{ids_regex}"}}[{days}d])"#, m.rx());
        self.traffic_by_label("peer_id", &download, &upload).await
    }

    /// One user's traffic on `metric` over the last `days` (WG figures include removed peers).
    pub async fn user_traffic(
        &self,
        metric: TrafficMetric,
        user_id: i64,
        days: u32,
    ) -> Result<Traffic> {
        let download = format!(
            r#"sum(increase({}{{user_id="{user_id}"}}[{days}d]))"#,
            metric.tx()
        );
        let upload = format!(
            r#"sum(increase({}{{user_id="{user_id}"}}[{days}d]))"#,
            metric.rx()
        );
        self.traffic_scalar(&download, &upload).await
    }

    /// Every user's traffic on `metric` over the last `days`, keyed by user id.
    pub async fn all_traffic(
        &self,
        metric: TrafficMetric,
        days: u32,
    ) -> Result<HashMap<i64, Traffic>> {
        let download = format!("sum by (user_id) (increase({}[{days}d]))", metric.tx());
        let upload = format!("sum by (user_id) (increase({}[{days}d]))", metric.rx());
        self.traffic_by_label("user_id", &download, &upload).await
    }

    /// System-wide total traffic (WG + VLESS) over the last `days`.
    pub async fn system_traffic(&self, days: u32) -> Result<Traffic> {
        let (wg, vless) = (TrafficMetric::Wg, TrafficMetric::Vless);
        let download = format!(
            "sum(increase({}[{days}d])) + sum(increase({}[{days}d]))",
            wg.tx(),
            vless.tx()
        );
        let upload = format!(
            "sum(increase({}[{days}d])) + sum(increase({}[{days}d]))",
            wg.rx(),
            vless.rx()
        );
        self.traffic_scalar(&download, &upload).await
    }
}
