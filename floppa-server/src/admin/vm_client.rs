//! VictoriaMetrics query client for reading traffic metrics.

use std::collections::HashMap;

use anyhow::{Context, Result};
use serde::Deserialize;

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

/// Execute a MetricsQL instant query against VictoriaMetrics.
async fn query_vm(
    client: &reqwest::Client,
    vm_url: &str,
    promql: &str,
) -> Result<Vec<(HashMap<String, String>, f64)>> {
    let resp: VmResponse = client
        .get(format!("{vm_url}/api/v1/query"))
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

/// Get per-peer WG traffic (tx, rx) for the given peer IDs over the last N days.
/// Returns peer_id -> (tx_bytes, rx_bytes).
pub async fn peer_traffic(
    client: &reqwest::Client,
    vm_url: &str,
    peer_ids: &[i64],
    days: u32,
) -> Result<HashMap<i64, (i64, i64)>> {
    if peer_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let ids_regex = peer_ids
        .iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join("|");
    let window = format!("{days}d");

    let tx_query = format!(r#"increase(wg_tx_bytes_total{{peer_id=~"{ids_regex}"}}[{window}])"#);
    let rx_query = format!(r#"increase(wg_rx_bytes_total{{peer_id=~"{ids_regex}"}}[{window}])"#);

    let (tx_results, rx_results) = tokio::try_join!(
        query_vm(client, vm_url, &tx_query),
        query_vm(client, vm_url, &rx_query),
    )?;

    let mut result: HashMap<i64, (i64, i64)> = HashMap::new();

    for (labels, value) in &tx_results {
        if let Some(pid) = labels.get("peer_id").and_then(|s| s.parse::<i64>().ok()) {
            result.entry(pid).or_default().0 = *value as i64;
        }
    }
    for (labels, value) in &rx_results {
        if let Some(pid) = labels.get("peer_id").and_then(|s| s.parse::<i64>().ok()) {
            result.entry(pid).or_default().1 = *value as i64;
        }
    }

    Ok(result)
}

/// Get system-wide total traffic (WG + VLESS) over the last N days.
/// Returns (total_tx, total_rx).
pub async fn system_traffic(
    client: &reqwest::Client,
    vm_url: &str,
    days: u32,
) -> Result<(i64, i64)> {
    let window = format!("{days}d");

    let tx_query = format!(
        "sum(increase(wg_tx_bytes_total[{window}])) + sum(increase(vless_tx_bytes_total[{window}]))"
    );
    let rx_query = format!(
        "sum(increase(wg_rx_bytes_total[{window}])) + sum(increase(vless_rx_bytes_total[{window}]))"
    );

    let (tx_results, rx_results) = tokio::try_join!(
        query_vm(client, vm_url, &tx_query),
        query_vm(client, vm_url, &rx_query),
    )?;

    let tx = tx_results.first().map(|(_, v)| *v as i64).unwrap_or(0);
    let rx = rx_results.first().map(|(_, v)| *v as i64).unwrap_or(0);

    Ok((tx, rx))
}
