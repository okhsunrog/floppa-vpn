//! Shared endpoint discovery. DNS results are candidates, never proof of server identity.
//! TLS and VPN public-key verification remain the caller's responsibility.
#[cfg(target_os = "android")]
pub mod android;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::watch;

const DNS_BUDGET: Duration = Duration::from_secs(4);
const STALE_LIMIT: u64 = 7 * 24 * 3600;
const MAX_HOSTS: usize = 128;

pub fn tls_config() -> rustls::ClientConfig {
    rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("supported TLS versions")
    .with_root_certificates(rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    })
    .with_no_client_auth()
}

pub fn client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .use_preconfigured_tls(tls_config())
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Entry {
    addresses: Vec<IpAddr>,
    expires: u64,
    good: Vec<(IpAddr, u64)>,
}
#[derive(Default)]
struct State {
    entries: HashMap<String, Entry>,
    failures: HashMap<(String, IpAddr), u64>,
    refreshing: HashMap<String, watch::Sender<Vec<IpAddr>>>,
    bootstrap: HashMap<String, Vec<IpAddr>>,
    network_epoch: u64,
}
pub struct Resolver {
    state: Mutex<State>,
    path: Option<PathBuf>,
    writer: tokio::sync::Mutex<()>,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn key(host: &str) -> String {
    host.trim_end_matches('.').to_ascii_lowercase()
}

impl Resolver {
    pub fn new(path: Option<PathBuf>) -> Arc<Self> {
        let entries = path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .filter(|b| b.len() <= 256 * 1024)
            .and_then(|b| serde_json::from_slice::<HashMap<String, Entry>>(&b).ok())
            .filter(|m| m.len() <= MAX_HOSTS)
            .unwrap_or_default();
        Arc::new(Self {
            state: Mutex::new(State {
                entries,
                ..State::default()
            }),
            path,
            writer: tokio::sync::Mutex::new(()),
        })
    }

    /// Immediate candidates, followed by a bounded refresh. Dropping the receiver does not
    /// interrupt another caller's refresh. At most one refresh runs per host.
    pub fn candidates(self: &Arc<Self>, host: &str) -> watch::Receiver<Vec<IpAddr>> {
        self.candidates_for(host, host)
    }

    pub fn candidates_for(
        self: &Arc<Self>,
        host: &str,
        identity: &str,
    ) -> watch::Receiver<Vec<IpAddr>> {
        self.discover(host, identity, false)
    }

    pub fn refresh_for(
        self: &Arc<Self>,
        host: &str,
        identity: &str,
    ) -> watch::Receiver<Vec<IpAddr>> {
        self.discover(host, identity, true)
    }

    fn discover(
        self: &Arc<Self>,
        host: &str,
        identity: &str,
        force: bool,
    ) -> watch::Receiver<Vec<IpAddr>> {
        let host = key(host);
        let identity = key(identity);
        if let Ok(ip) = host.parse() {
            return watch::channel(vec![ip]).1;
        }
        let mut state = self.state.lock().unwrap();
        if let Some(rx) = state.refreshing.get(&identity) {
            return rx.subscribe();
        }
        let cached = Self::cached(&state, &identity);
        if !force
            && !cached.is_empty()
            && state
                .entries
                .get(&identity)
                .is_some_and(|entry| entry.expires > now())
        {
            return watch::channel(cached).1;
        }
        tracing::debug!(%host, cached = cached.len(), "resolving endpoint");
        let (tx, rx) = watch::channel(cached);
        state.refreshing.insert(identity.clone(), tx.clone());
        let this = self.clone();
        let epoch = state.network_epoch;
        tokio::spawn(async move {
            let mut jobs = tokio::task::JoinSet::new();
            let name = host.clone();
            jobs.spawn(async move {
                let ips = match tokio::net::lookup_host((name.as_str(), 0)).await {
                    Ok(addresses) => addresses.map(|a| a.ip()).collect::<Vec<_>>(),
                    Err(error) => {
                        tracing::debug!(host = %name, %error, "system DNS failed");
                        return None;
                    }
                };
                // getaddrinfo does not expose TTL. Keep its answers only briefly.
                Some((ips, 60, "system"))
            });
            for (url, bootstrap) in [
                ("https://dns.google/resolve", "8.8.8.8"),
                ("https://cloudflare-dns.com/dns-query", "1.1.1.1"),
            ] {
                for kind in [1, 28] {
                    let name = host.clone();
                    jobs.spawn(async move {
                        tokio::time::sleep(Duration::from_millis(650)).await;
                        match doh(&name, kind, url, bootstrap).await {
                            Ok((ips, ttl)) => Some((ips, ttl, url)),
                            Err(detail) => { tracing::debug!(host = %name, resolver = url, %detail, "DNS fallback failed"); None }
                        }
                    });
                }
            }
            let deadline = tokio::time::Instant::now() + DNS_BUDGET;
            let mut fresh = Vec::new();
            let mut ttl = u64::MAX;
            while let Ok(Some(result)) = tokio::time::timeout_at(deadline, jobs.join_next()).await {
                if let Ok(Some((ips, lifetime, source))) = result {
                    if ips.is_empty() {
                        continue;
                    }
                    tracing::debug!(%host, source, count = ips.len(), "received endpoint candidates");
                    ttl = ttl.min(lifetime);
                    for ip in ips {
                        if !fresh.contains(&ip) {
                            fresh.push(ip);
                        }
                    }
                    fresh.truncate(16);
                    let mut state = this.state.lock().unwrap();
                    if state.network_epoch != epoch {
                        break;
                    }
                    if state.entries.len() >= MAX_HOSTS
                        && !state.entries.contains_key(&identity)
                        && let Some(old) = state.entries.keys().next().cloned()
                    {
                        state.entries.remove(&old);
                    }
                    let entry = state.entries.entry(identity.clone()).or_default();
                    entry.addresses = fresh.clone();
                    entry.expires = now().saturating_add(ttl.min(3600));
                    let mut candidates = Self::cached(&state, &identity);
                    // TTL=0 answers are usable by this request but not subsequent ones.
                    for ip in &fresh {
                        if !candidates.contains(ip) {
                            candidates.push(*ip);
                        }
                    }
                    tx.send_replace(candidates);
                }
            }
            jobs.abort_all();
            {
                let mut state = this.state.lock().unwrap();
                if state.network_epoch == epoch {
                    state.refreshing.remove(&identity);
                }
            }
            this.persist().await;
        });
        rx
    }

    fn cached(state: &State, host: &str) -> Vec<IpAddr> {
        let mut out = Vec::new();
        if let Some(e) = state.entries.get(host) {
            for (ip, time) in &e.good {
                if now().saturating_sub(*time) <= STALE_LIMIT && !out.contains(ip) {
                    out.push(*ip);
                }
            }
            if e.expires > now() {
                for ip in &e.addresses {
                    if !out.contains(ip) {
                        out.push(*ip);
                    }
                }
            }
        }
        if let Some(ips) = state.bootstrap.get(host) {
            for ip in ips {
                if !out.contains(ip) {
                    out.push(*ip);
                }
            }
        }
        out.sort_by_key(|ip| {
            state
                .failures
                .get(&(host.to_owned(), *ip))
                .is_some_and(|t| now().saturating_sub(*t) < 60)
        });
        out
    }

    /// Optional installation-provided initial addresses. These remain unverified candidates.
    pub fn set_bootstrap(&self, identity: &str, addresses: Vec<IpAddr>) {
        let mut state = self.state.lock().unwrap();
        if state.bootstrap.len() < MAX_HOSTS {
            state
                .bootstrap
                .insert(key(identity), addresses.into_iter().take(16).collect());
        }
    }

    /// A platform-specific underlay resolver can contribute candidates without marking them
    /// verified. Preserve the shared failure ranking and never persist these as successful.
    pub fn observe(&self, host: &str, addresses: Vec<IpAddr>, ttl: u64) {
        let host = key(host);
        let mut state = self.state.lock().unwrap();
        if state.entries.len() >= MAX_HOSTS && !state.entries.contains_key(&host) {
            return;
        }
        let entry = state.entries.entry(host.clone()).or_default();
        entry.addresses = addresses.into_iter().take(16).collect();
        entry.expires = now().saturating_add(ttl.min(3600));
        if let Some(tx) = state.refreshing.get(&host) {
            tx.send_replace(Self::cached(&state, &host));
        }
    }

    pub async fn resolve(self: &Arc<Self>, host: &str) -> io::Result<Vec<IpAddr>> {
        let cached = Self::cached(&self.state.lock().unwrap(), &key(host));
        let mut rx = self.candidates(host);
        if !cached.is_empty() {
            return Ok(cached);
        }
        loop {
            let ips = rx.borrow_and_update().clone();
            if !ips.is_empty() {
                return Ok(ips);
            }
            if rx.changed().await.is_err() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "system DNS and encrypted DNS returned no usable addresses",
                ));
            }
        }
    }

    /// Call only after authenticated TLS/HTTP or a verified VPN handshake.
    pub async fn remember(self: &Arc<Self>, host: &str, ip: IpAddr) {
        {
            let mut state = self.state.lock().unwrap();
            let host = key(host);
            if state.entries.len() >= MAX_HOSTS && !state.entries.contains_key(&host) {
                return;
            }
            let entry = state.entries.entry(host.clone()).or_default();
            entry.good.retain(|(old, _)| *old != ip);
            entry.good.insert(0, (ip, now()));
            entry.good.truncate(4);
            state.failures.remove(&(host, ip));
        }
        self.persist().await;
    }

    pub fn failed(&self, host: &str, ip: IpAddr) {
        let mut state = self.state.lock().unwrap();
        state.failures.retain(|_, t| now().saturating_sub(*t) < 60);
        state.failures.insert((key(host), ip), now());
    }

    async fn persist(&self) {
        let _writer = self.writer.lock().await;
        let Some(path) = self.path.clone() else {
            return;
        };
        let Ok(bytes) = serde_json::to_vec(&self.state.lock().unwrap().entries) else {
            return;
        };
        let _ = tokio::task::spawn_blocking(move || -> io::Result<()> {
            let parent = path
                .parent()
                .ok_or_else(|| io::Error::other("cache directory missing"))?;
            std::fs::create_dir_all(parent)?;
            let mut file = tempfile::NamedTempFile::new_in(parent)?;
            use std::io::Write;
            file.write_all(&bytes)?;
            file.persist(path).map_err(io::Error::other)?;
            Ok(())
        })
        .await;
    }
}

#[derive(Deserialize)]
struct DnsAnswer {
    #[serde(rename = "type")]
    kind: u16,
    #[serde(rename = "TTL")]
    ttl: u64,
    data: String,
}
#[derive(Deserialize)]
struct DnsResponse {
    #[serde(rename = "Status")]
    status: u16,
    #[serde(rename = "Answer", default)]
    answers: Vec<DnsAnswer>,
}
#[derive(Debug)]
struct SendError {
    detail: String,
    connect: bool,
}
impl From<reqwest::Error> for SendError {
    fn from(error: reqwest::Error) -> Self {
        Self {
            connect: error.is_connect(),
            detail: error.without_url().to_string(),
        }
    }
}
async fn send_to(
    request: reqwest::Request,
    ip: IpAddr,
    _underlay_only: bool,
) -> Result<reqwest::Response, SendError> {
    let port = request.url().port_or_known_default().unwrap();
    #[cfg(not(target_os = "android"))]
    {
        let host = request.url().host_str().unwrap();
        let client = client_builder()
            .no_proxy()
            .resolve(host, SocketAddr::new(ip, port))
            .build()?;
        Ok(client.execute(request).await?)
    }
    #[cfg(target_os = "android")]
    {
        if !_underlay_only {
            // A working VPN may be the only route to the API. Preserve that route first;
            // fall back to the physical network only if no HTTP request could be sent.
            let client = client_builder()
                .no_proxy()
                .resolve(request.url().host_str().unwrap(), SocketAddr::new(ip, port))
                .build()?;
            let attempt = request.try_clone().ok_or_else(|| SendError {
                connect: false,
                detail: "streaming request cannot be retried".into(),
            })?;
            match client.execute(attempt).await {
                Ok(response) => return Ok(response),
                Err(error) if error.is_connect() => {}
                Err(error) => return Err(error.into()),
            }
        }
        let (client, guard) =
            android::client(SocketAddr::new(ip, port))
                .await
                .map_err(|error| SendError {
                    connect: true,
                    detail: error.to_string(),
                })?;
        let response = client.execute(request).await?;
        // Keep the relay alive with the BODY, not response extensions: reqwest's text/json
        // consumers discard extensions before they finish reading the body.
        use reqwest::ResponseBuilderExt;
        let mut builder = http::Response::builder()
            .status(response.status())
            .version(response.version())
            .url(response.url().clone());
        *builder.headers_mut().unwrap() = response.headers().clone();
        let stream = futures_util::stream::try_unfold(
            (response, guard),
            |(mut response, guard)| async move {
                response
                    .chunk()
                    .await
                    .map(|chunk| chunk.map(|chunk| (chunk, (response, guard))))
            },
        );
        Ok(builder
            .body(reqwest::Body::wrap_stream(stream))
            .expect("valid upstream response")
            .into())
    }
}

async fn doh(
    host: &str,
    kind: u16,
    endpoint: &str,
    bootstrap: &str,
) -> Result<(Vec<IpAddr>, u64), String> {
    let mut url = reqwest::Url::parse(endpoint).map_err(|e| e.to_string())?;
    url.query_pairs_mut()
        .append_pair("name", host)
        .append_pair("type", &kind.to_string());
    let request = client_builder()
        .build()
        .map_err(|e| e.to_string())?
        .get(url)
        .header("accept", "application/dns-json")
        .build()
        .map_err(|e| e.to_string())?;
    let mut response = send_to(request, bootstrap.parse().unwrap(), true)
        .await
        .map_err(|e| e.detail)?
        .error_for_status()
        .map_err(|e| e.without_url().to_string())?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| e.without_url().to_string())?
    {
        if bytes.len() + chunk.len() > 65536 {
            return Err("DNS response too large".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    let response: DnsResponse = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if response.status != 0 {
        return Err(format!("DNS status {}", response.status));
    }
    let mut ips = Vec::new();
    let mut ttl = 3600;
    for a in response.answers {
        ttl = ttl.min(a.ttl);
        if a.kind == kind
            && let Ok(ip) = a.data.parse::<IpAddr>()
            && ((kind == 1 && ip.is_ipv4()) || (kind == 28 && ip.is_ipv6()))
        {
            ips.push(ip);
        }
    }
    Ok((ips, ttl))
}

static GLOBAL: OnceLock<Arc<Resolver>> = OnceLock::new();
/// Android initializes this with its private files directory before the first request.
pub fn init(path: PathBuf) {
    let _ = GLOBAL.set(configured_resolver(Some(path)));
}
/// Called by the platform when its underlay changes. Last verified addresses remain candidates,
/// but DNS answers and failure penalties from the old network are no longer authoritative.
pub fn network_changed() {
    if let Some(resolver) = GLOBAL.get() {
        let mut state = resolver.state.lock().unwrap();
        state.network_epoch = state.network_epoch.wrapping_add(1);
        state.refreshing.clear();
        state.failures.clear();
        for entry in state.entries.values_mut() {
            entry.expires = 0;
        }
    }
}

fn configured_resolver(path: Option<PathBuf>) -> Arc<Resolver> {
    let resolver = Resolver::new(path);
    // Public addresses only, supplied at build time for deployments needing DNS-free first start.
    // Example: {"https://vpn.example:443":["192.0.2.1","192.0.2.2"]}
    if let Some(config) = option_env!("FLOPPA_API_BOOTSTRAP") {
        if let Ok(entries) = serde_json::from_str::<HashMap<String, Vec<IpAddr>>>(config) {
            for (identity, addresses) in entries {
                resolver.set_bootstrap(&identity, addresses);
            }
        } else {
            tracing::warn!("invalid FLOPPA_API_BOOTSTRAP configuration");
        }
    }
    resolver
}

pub fn resolver() -> Arc<Resolver> {
    GLOBAL
        .get_or_init(|| {
            configured_resolver(dirs::cache_dir().map(|p| p.join("floppa-vpn/endpoints.json")))
        })
        .clone()
}

pub fn split_endpoint(endpoint: &str) -> Result<(String, u16), String> {
    let (host, port) = endpoint.rsplit_once(':').ok_or("endpoint needs a port")?;
    let host = host
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(host);
    if host.is_empty() {
        return Err("endpoint needs a host".into());
    }
    Ok((
        host.into(),
        port.parse().map_err(|_| "invalid endpoint port")?,
    ))
}

/// Only connect errors are retried: once HTTP bytes may have been sent, replaying a POST can
/// duplicate a purchase, consume a login code, or create a second peer.
pub async fn execute(request: reqwest::Request) -> Result<reqwest::Response, String> {
    tokio::time::timeout(Duration::from_secs(15), execute_inner(request))
        .await
        .map_err(|_| "request exceeded its connection budget".to_owned())?
}
async fn execute_inner(request: reqwest::Request) -> Result<reqwest::Response, String> {
    let host = request
        .url()
        .host_str()
        .ok_or("request has no host")?
        .trim_matches(['[', ']'])
        .to_owned();
    let port = request
        .url()
        .port_or_known_default()
        .ok_or("unsupported URL scheme")?;
    let resolver = resolver();
    let identity = format!("{}://{}:{}", request.url().scheme(), host, port);
    let candidates = resolver.candidates_for(&host, &identity);
    let retry = request.try_clone();
    match try_candidates(request, identity.clone(), resolver.clone(), candidates).await {
        Err(error) if error.connect => {
            let Some(request) = retry else {
                return Err(error.detail);
            };
            let candidates = resolver.refresh_for(&host, &identity);
            try_candidates(request, identity, resolver, candidates)
                .await
                .map_err(|e| e.detail)
        }
        result => result.map_err(|e| e.detail),
    }
}
async fn try_candidates(
    request: reqwest::Request,
    host: String,
    resolver: Arc<Resolver>,
    mut candidates: watch::Receiver<Vec<IpAddr>>,
) -> Result<reqwest::Response, SendError> {
    let mut tried = Vec::new();
    let mut last_error = "no usable addresses from system DNS, encrypted DNS or cache".to_owned();
    loop {
        let addresses = candidates.borrow_and_update().clone();
        for ip in addresses {
            if tried.contains(&ip) {
                continue;
            }
            tried.push(ip);
            let attempt = request.try_clone().ok_or_else(|| SendError {
                connect: false,
                detail: "streaming requests cannot be retried".into(),
            })?;
            match send_to(attempt, ip, false).await {
                Ok(response) => {
                    if response.url().scheme() == "https" {
                        resolver.remember(&host, ip).await;
                    }
                    return Ok(response);
                }
                Err(e) => {
                    // Do not expose URLs containing login codes in diagnostics.
                    let connect_error = e.connect;
                    last_error = e.detail;
                    if !connect_error {
                        return Err(SendError {
                            connect: false,
                            detail: last_error,
                        });
                    }
                    resolver.failed(&host, ip);
                    tracing::debug!(%ip, "connection failed, trying next endpoint");
                }
            }
        }
        if candidates.changed().await.is_err() {
            return Err(SendError {
                connect: true,
                detail: last_error,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn literals_need_no_dns() {
        let r = Resolver::new(None);
        assert_eq!(
            r.resolve("::1").await.unwrap(),
            vec!["::1".parse::<IpAddr>().unwrap()]
        );
    }
    #[tokio::test]
    async fn only_verified_stale_addresses_survive_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.json");
        let r = Resolver::new(Some(path.clone()));
        let ip = "192.0.2.1".parse().unwrap();
        r.remember("Example.COM.", ip).await;
        let restored = Resolver::new(Some(path));
        assert_eq!(
            Resolver::cached(&restored.state.lock().unwrap(), "example.com"),
            vec![ip]
        );
    }
    #[test]
    fn expiry_and_failure_priority() {
        let mut s = State::default();
        let a = "192.0.2.1".parse().unwrap();
        let b = "192.0.2.2".parse().unwrap();
        s.entries.insert(
            "test".into(),
            Entry {
                addresses: vec![b],
                expires: now() - 1,
                good: vec![(a, now())],
            },
        );
        assert_eq!(Resolver::cached(&s, "test"), vec![a]);
        s.entries.get_mut("test").unwrap().expires = now() + 60;
        s.failures.insert(("test".into(), a), now());
        assert_eq!(Resolver::cached(&s, "test"), vec![b, a]);
    }
}

#[cfg(test)]
mod transport_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn connection_refusal_tries_next_ip_and_preserves_host_and_body() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut buf = [0; 1024];
                let n = socket.read(&mut buf).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buf[..n]);
                if bytes.ends_with(b"payload") {
                    break;
                }
            }
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .await
                .unwrap();
            String::from_utf8(bytes).unwrap()
        });
        let request = client_builder()
            .build()
            .unwrap()
            .post(format!("http://example.invalid:{port}/api"))
            .body("payload")
            .build()
            .unwrap();
        let rx = watch::channel(vec![
            "127.0.0.2".parse().unwrap(),
            "127.0.0.1".parse().unwrap(),
        ])
        .1;
        let response = try_candidates(request, "test".into(), Resolver::new(None), rx)
            .await
            .unwrap();
        assert_eq!(response.text().await.unwrap(), "ok");
        let received = server.await.unwrap();
        assert!(received.contains(&format!("host: example.invalid:{port}")));
        assert!(received.starts_with("POST /api HTTP/1.1"));
    }

    #[tokio::test]
    async fn lost_response_does_not_replay_post() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0; 4096];
            assert!(socket.read(&mut buf).await.unwrap() > 0);
            drop(socket);
            tokio::time::timeout(Duration::from_millis(200), listener.accept())
                .await
                .is_err()
        });
        let request = client_builder()
            .build()
            .unwrap()
            .post(format!("http://example.invalid:{port}/api"))
            .body("payload")
            .build()
            .unwrap();
        let rx = watch::channel(vec![
            "127.0.0.1".parse().unwrap(),
            "127.0.0.2".parse().unwrap(),
        ])
        .1;
        assert!(
            try_candidates(request, "test".into(), Resolver::new(None), rx)
                .await
                .is_err()
        );
        assert!(server.await.unwrap());
    }

    #[tokio::test]
    async fn addresses_arriving_after_initial_failure_are_tried() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0; 4096];
            assert!(socket.read(&mut buf).await.unwrap() > 0);
            socket
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
        });
        let (tx, rx) = watch::channel(vec!["127.0.0.2".parse().unwrap()]);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            tx.send_replace(vec!["127.0.0.1".parse().unwrap()]);
        });
        let request = client_builder()
            .build()
            .unwrap()
            .get(format!("http://example.invalid:{port}/api"))
            .build()
            .unwrap();
        assert_eq!(
            try_candidates(request, "test".into(), Resolver::new(None), rx)
                .await
                .unwrap()
                .status(),
            204
        );
        server.await.unwrap();
    }
}

#[cfg(test)]
mod tls_tests {
    use super::*;
    #[tokio::test]
    async fn pinned_ip_does_not_disable_certificate_verification() {
        let generated = rcgen::generate_simple_self_signed(vec!["example.invalid".into()]).unwrap();
        let key =
            rustls::pki_types::PrivatePkcs8KeyDer::from(generated.signing_key.serialize_der());
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![generated.cert.der().clone()], key.into())
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let _ = tokio_rustls::TlsAcceptor::from(Arc::new(config))
                .accept(socket)
                .await;
        });
        let req = client_builder()
            .build()
            .unwrap()
            .get(format!("https://example.invalid:{port}/"))
            .build()
            .unwrap();
        let result = send_to(req, "127.0.0.1".parse().unwrap(), false).await;
        assert!(
            result.is_err(),
            "an untrusted certificate must never be accepted even for a cached IP"
        );
        server.await.unwrap();
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    #[tokio::test]
    async fn fresh_dns_cache_avoids_network_requests() {
        let r = Resolver::new(None);
        let ip = "192.0.2.1".parse().unwrap();
        r.observe("server-key:443", vec![ip], 60);
        let rx = r.candidates_for("example.invalid", "server-key:443");
        assert_eq!(*rx.borrow(), vec![ip]);
        assert!(
            rx.has_changed().is_err(),
            "fresh cache should close without starting a resolver task"
        );
    }
    #[test]
    fn expired_verified_address_and_other_identity_are_not_reused() {
        let mut state = State::default();
        state.entries.insert(
            "old-key".into(),
            Entry {
                good: vec![("192.0.2.1".parse().unwrap(), now() - STALE_LIMIT - 1)],
                ..Entry::default()
            },
        );
        assert!(Resolver::cached(&state, "old-key").is_empty());
        assert!(Resolver::cached(&state, "new-key").is_empty());
    }
    #[test]
    fn corrupt_cache_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("cache.json");
        std::fs::write(&file, "{broken").unwrap();
        let r = Resolver::new(Some(file));
        assert!(r.state.lock().unwrap().entries.is_empty());
    }
}
