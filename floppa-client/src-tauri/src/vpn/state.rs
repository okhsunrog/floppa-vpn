use super::protocol::{Preference, Protocol};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use ipnetwork::IpNetwork;
use serde::{Deserialize, Serialize};
use shoes_lite::api::VlessConfig;
use specta::Type;
use std::net::IpAddr;
use std::str::FromStr;

/// Why a config could not be read.
///
/// Every value that is later parsed for real — addresses, DNS servers, allowed IPs, the MTU, the
/// AmneziaWG numbers, the keys — is checked here, at import, so a typo is an error the user sees
/// instead of an entry silently dropped by a `filter_map(Result::ok)` deep in the connect path.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigParseError {
    #[error("line {line}: `{text}` is not `Key = Value`")]
    NotKeyValue { line: usize, text: String },
    #[error("line {line}: `{key}` appears before any section")]
    OutsideSection { line: usize, key: String },
    #[error("line {line}: unknown section [{name}]")]
    UnknownSection { line: usize, name: String },
    #[error("[{section}] is missing `{key}`")]
    Missing {
        section: &'static str,
        key: &'static str,
    },
    #[error("line {line}: `{key} = {value}`: {detail}")]
    InvalidValue {
        line: usize,
        key: String,
        value: String,
        detail: String,
    },
    #[error("invalid VLESS URI: {0}")]
    Vless(String),
}

/// A WireGuard key that does not decode to 32 bytes.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    #[error("not base64: {0}")]
    Base64(#[from] base64::DecodeError),
    #[error("a key is 32 bytes, this one decodes to {0}")]
    Length(usize),
}

fn decode_key(encoded: &str) -> Result<[u8; 32], KeyError> {
    let bytes = BASE64.decode(encoded)?;
    let len = bytes.len();
    bytes.try_into().map_err(|_| KeyError::Length(len))
}

/// One `Key = Value` line, with the key lower-cased for matching and the line kept for errors.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    line: usize,
    key: String,
    value: String,
}

/// The two sections a WireGuard-family `.conf` may contain. Unknown *keys* are tolerated — the
/// format has many we do not model (`ListenPort`, `Table`, `PreUp`…) — unknown *sections* are not.
#[derive(Debug, Default, PartialEq, Eq)]
struct Sections {
    interface: Vec<Entry>,
    peer: Vec<Entry>,
}

impl Sections {
    /// The last value for `key` in `entries`: a repeated key overrides, as `wg setconf` does.
    fn last<'a>(entries: &'a [Entry], key: &str) -> Option<&'a Entry> {
        entries.iter().rev().find(|e| e.key == key)
    }

    fn interface(&self, key: &str) -> Option<&Entry> {
        Self::last(&self.interface, key)
    }

    fn peer(&self, key: &str) -> Option<&Entry> {
        Self::last(&self.peer, key)
    }
}

/// The one pass over the text. Strict about shape, lenient about vocabulary.
fn parse_sections(config: &str) -> Result<Sections, ConfigParseError> {
    #[derive(Clone, Copy)]
    enum Section {
        Interface,
        Peer,
    }

    let mut sections = Sections::default();
    let mut current = None;

    for (index, raw) in config.lines().enumerate() {
        let line = index + 1;
        let text = raw.trim();
        if text.is_empty() || text.starts_with('#') {
            continue;
        }
        if let Some(name) = text.strip_prefix('[').and_then(|t| t.strip_suffix(']')) {
            current = Some(match name.trim() {
                n if n.eq_ignore_ascii_case("interface") => Section::Interface,
                n if n.eq_ignore_ascii_case("peer") => Section::Peer,
                n => {
                    return Err(ConfigParseError::UnknownSection {
                        line,
                        name: n.to_string(),
                    });
                }
            });
            continue;
        }
        let Some((key, value)) = text.split_once('=') else {
            return Err(ConfigParseError::NotKeyValue {
                line,
                text: text.to_string(),
            });
        };
        let entry = Entry {
            line,
            key: key.trim().to_ascii_lowercase(),
            value: value.trim().to_string(),
        };
        match current {
            Some(Section::Interface) => sections.interface.push(entry),
            Some(Section::Peer) => sections.peer.push(entry),
            None => {
                return Err(ConfigParseError::OutsideSection {
                    line,
                    key: entry.key,
                });
            }
        }
    }
    Ok(sections)
}

/// Check that an entry's value parses as `T`, keeping the text: the stored form is the text.
fn checked<T: FromStr>(entry: &Entry) -> Result<String, ConfigParseError>
where
    T::Err: std::fmt::Display,
{
    entry
        .value
        .parse::<T>()
        .map_err(|e| invalid(entry, e))
        .map(|_| entry.value.clone())
}

/// Parse an entry's value as `T`.
fn parsed<T: FromStr>(entry: &Entry) -> Result<T, ConfigParseError>
where
    T::Err: std::fmt::Display,
{
    entry.value.parse::<T>().map_err(|e| invalid(entry, e))
}

/// Check every comma-separated item of an entry's value parses as `T`.
fn checked_list<T: FromStr>(entry: &Entry) -> Result<String, ConfigParseError>
where
    T::Err: std::fmt::Display,
{
    for item in entry.value.split(',') {
        item.trim().parse::<T>().map_err(|e| invalid(entry, e))?;
    }
    Ok(entry.value.clone())
}

/// One item of a `DNS =` line, in wg-quick's reading of it: an IP address is a resolver, anything
/// else is a search domain.
///
/// Both kinds are kept as the text they came in as. The search domains are not applied to the
/// tunnel's resolver configuration yet; keeping them is what makes that possible later, and what
/// lets a `.conf` written for wg-quick import at all — the strict parser used to reject the whole
/// file over a `corp.example` it would then have ignored anyway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DnsEntry {
    Server(IpAddr),
    SearchDomain(String),
}

impl FromStr for DnsEntry {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Ok(ip) = s.parse::<IpAddr>() {
            return Ok(Self::Server(ip));
        }
        if is_plausible_hostname(s) {
            return Ok(Self::SearchDomain(s.to_string()));
        }
        Err(format!(
            "`{s}` is neither an IP address nor a search domain"
        ))
    }
}

/// The shape of a hostname (RFC 1123): dot-separated labels of ASCII letters, digits and hyphens,
/// none empty, longer than 63 or starting or ending with a hyphen — and the last label not all
/// digits, so a mistyped address such as `1.1.1` is not waved through as a domain.
fn is_plausible_hostname(s: &str) -> bool {
    let s = s.strip_suffix('.').unwrap_or(s);
    if s.is_empty() || s.len() > 253 {
        return false;
    }
    let label_ok = |label: &str| {
        !label.is_empty()
            && label.len() <= 63
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    };
    let mut labels = s.split('.').peekable();
    let mut last_all_digits = false;
    while let Some(label) = labels.next() {
        if !label_ok(label) {
            return false;
        }
        if labels.peek().is_none() {
            last_all_digits = label.bytes().all(|b| b.is_ascii_digit());
        }
    }
    !last_all_digits
}

/// The entries of a stored `DNS =` line. The text was checked at import, so an item that no
/// longer parses can only come from an older store; it is skipped.
fn dns_entries(dns: Option<&str>) -> Vec<DnsEntry> {
    dns.map(|dns| {
        dns.split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect()
    })
    .unwrap_or_default()
}

fn dns_servers_of(dns: Option<&str>) -> Vec<IpAddr> {
    dns_entries(dns)
        .into_iter()
        .filter_map(|entry| match entry {
            DnsEntry::Server(ip) => Some(ip),
            DnsEntry::SearchDomain(_) => None,
        })
        .collect()
}

fn checked_key(entry: &Entry) -> Result<String, ConfigParseError> {
    decode_key(&entry.value).map_err(|e| invalid(entry, e))?;
    Ok(entry.value.clone())
}

/// `host:port`, with the port a real port. The host is resolved later, by the attempt.
fn checked_endpoint(entry: &Entry) -> Result<String, ConfigParseError> {
    match entry.value.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && port.parse::<u16>().is_ok() => {
            Ok(entry.value.clone())
        }
        _ => Err(invalid(entry, "expected host:port")),
    }
}

fn invalid(entry: &Entry, detail: impl std::fmt::Display) -> ConfigParseError {
    ConfigParseError::InvalidValue {
        line: entry.line,
        key: entry.key.clone(),
        value: entry.value.clone(),
        detail: detail.to_string(),
    }
}

fn required<'a>(
    entry: Option<&'a Entry>,
    section: &'static str,
    key: &'static str,
) -> Result<&'a Entry, ConfigParseError> {
    entry.ok_or(ConfigParseError::Missing { section, key })
}

/// WireGuard configuration
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct WgConfig {
    pub private_key: String,
    pub address: String,
    pub dns: Option<String>,
    pub mtu: Option<u16>,
    pub peer_public_key: String,
    pub peer_preshared_key: Option<String>,
    pub peer_endpoint: String,
    pub allowed_ips: String,
    pub persistent_keepalive: Option<u16>,
}

impl WgConfig {
    /// Get private key as 32-byte array for gotatun
    pub fn private_key_bytes(&self) -> Result<[u8; 32], KeyError> {
        decode_key(&self.private_key)
    }

    /// Get peer public key as 32-byte array for gotatun
    pub fn peer_public_key_bytes(&self) -> Result<[u8; 32], KeyError> {
        decode_key(&self.peer_public_key)
    }

    /// Get peer preshared key as 32-byte array for gotatun (if set)
    pub fn peer_preshared_key_bytes(&self) -> Result<Option<[u8; 32]>, KeyError> {
        self.peer_preshared_key
            .as_deref()
            .map(decode_key)
            .transpose()
    }

    /// Get address as IpNetwork
    pub fn address_network(&self) -> Result<IpNetwork, ipnetwork::IpNetworkError> {
        IpNetwork::from_str(&self.address)
    }

    /// Everything on the `DNS =` line: resolvers and search domains, in order.
    pub fn dns_entries(&self) -> Vec<DnsEntry> {
        dns_entries(self.dns.as_deref())
    }

    /// The resolvers only. Search domains are not applied to the tunnel yet.
    pub fn dns_servers(&self) -> Vec<IpAddr> {
        dns_servers_of(self.dns.as_deref())
    }

    /// The search domains only.
    pub fn dns_search_domains(&self) -> Vec<String> {
        self.dns_entries()
            .into_iter()
            .filter_map(|entry| match entry {
                DnsEntry::SearchDomain(domain) => Some(domain),
                DnsEntry::Server(_) => None,
            })
            .collect()
    }

    /// Get allowed IPs as `Vec<IpNetwork>`
    pub fn allowed_ips_networks(&self) -> Vec<IpNetwork> {
        self.allowed_ips
            .split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect()
    }

    /// Get MTU (default 1420 for WireGuard)
    pub fn get_mtu(&self) -> u16 {
        self.mtu.unwrap_or(1420)
    }
}

impl WgConfig {
    /// Parse from WireGuard config file format.
    pub fn from_config_str(config: &str) -> Result<Self, ConfigParseError> {
        Self::from_sections(&parse_sections(config)?)
    }

    fn from_sections(sections: &Sections) -> Result<Self, ConfigParseError> {
        Ok(WgConfig {
            private_key: checked_key(required(
                sections.interface("privatekey"),
                "Interface",
                "PrivateKey",
            )?)?,
            address: checked::<IpNetwork>(required(
                sections.interface("address"),
                "Interface",
                "Address",
            )?)?,
            dns: sections
                .interface("dns")
                .map(checked_list::<DnsEntry>)
                .transpose()?,
            mtu: sections.interface("mtu").map(parsed::<u16>).transpose()?,
            peer_public_key: checked_key(required(
                sections.peer("publickey"),
                "Peer",
                "PublicKey",
            )?)?,
            peer_preshared_key: sections.peer("presharedkey").map(checked_key).transpose()?,
            peer_endpoint: checked_endpoint(required(
                sections.peer("endpoint"),
                "Peer",
                "Endpoint",
            )?)?,
            allowed_ips: match sections.peer("allowedips") {
                Some(entry) => checked_list::<IpNetwork>(entry)?,
                None => "0.0.0.0/0, ::/0".to_string(),
            },
            persistent_keepalive: sections
                .peer("persistentkeepalive")
                .map(parsed::<u16>)
                .transpose()?,
        })
    }
}

/// Tracks previous stats for computing transfer rates
pub struct SpeedTracker {
    prev_tx_bytes: u64,
    prev_rx_bytes: u64,
    prev_time: std::time::Instant,
    has_baseline: bool,
}

impl SpeedTracker {
    pub fn new() -> Self {
        Self {
            prev_tx_bytes: 0,
            prev_rx_bytes: 0,
            prev_time: std::time::Instant::now(),
            has_baseline: false,
        }
    }

    /// Update with new cumulative byte counts and return computed speeds (bytes/sec)
    pub fn update(&mut self, tx_bytes: u64, rx_bytes: u64) -> (f64, f64) {
        let now = std::time::Instant::now();

        // First sample after reset: just store the baseline, don't compute speed.
        // Without this, reconnecting to an already-running tunnel would divide
        // the full cumulative byte count by a tiny elapsed time → huge spike.
        if !self.has_baseline {
            self.prev_tx_bytes = tx_bytes;
            self.prev_rx_bytes = rx_bytes;
            self.prev_time = now;
            self.has_baseline = true;
            return (0.0, 0.0);
        }

        let elapsed = now.duration_since(self.prev_time).as_secs_f64();

        let (tx_speed, rx_speed) = if elapsed > 0.1 {
            let tx_delta = tx_bytes.saturating_sub(self.prev_tx_bytes);
            let rx_delta = rx_bytes.saturating_sub(self.prev_rx_bytes);
            (tx_delta as f64 / elapsed, rx_delta as f64 / elapsed)
        } else {
            (0.0, 0.0)
        };

        self.prev_tx_bytes = tx_bytes;
        self.prev_rx_bytes = rx_bytes;
        self.prev_time = now;

        (tx_speed, rx_speed)
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

impl Default for SpeedTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// VLESS config with VPN-specific fields (address, dns, routes).
///
/// Wraps the core VLESS connection parameters from the URI together with
/// tunnel configuration (IP address, DNS, routing) needed for VPN operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VlessVpnConfig {
    /// Original VLESS URI (for persistence)
    pub uri: String,
    pub uuid: String,
    /// Server address as "host:port"
    pub server_addr: String,
    /// SNI hostname for REALITY handshake
    pub server_name: String,
    /// REALITY public key (base64url-no-pad)
    pub reality_public_key: String,
    /// REALITY short ID (hex)
    pub reality_short_id: String,
    /// Flow control mode, e.g. "xtls-rprx-vision"
    pub flow: Option<String>,
    /// Tunnel IP address with CIDR prefix, e.g. "10.0.0.2/32"
    pub address: String,
    /// DNS servers, comma-separated
    pub dns: Option<String>,
    /// TUN MTU (default 1500)
    pub mtu: Option<u16>,
    /// Allowed IPs for routing, comma-separated CIDRs
    pub allowed_ips: String,
}

impl VlessVpnConfig {
    /// Parse a VLESS URI and fill VPN-specific fields with defaults.
    pub fn from_uri(uri: &str) -> Result<Self, ConfigParseError> {
        let parsed = VlessConfig::from_uri(uri).map_err(ConfigParseError::Vless)?;
        Ok(Self {
            uri: uri.to_string(),
            uuid: parsed.uuid,
            server_addr: parsed.server_addr,
            server_name: parsed.server_name,
            reality_public_key: parsed.reality_public_key,
            reality_short_id: parsed.reality_short_id,
            flow: parsed.flow,
            address: "10.0.0.2/32".to_string(),
            dns: Some("1.1.1.1".to_string()),
            mtu: Some(1500),
            allowed_ips: "0.0.0.0/0, ::/0".to_string(),
        })
    }

    /// Convert to the shoes library VlessConfig for tunnel creation.
    pub fn to_shoes_config(&self) -> VlessConfig {
        let address = self.address.split('/').next().map(|s| s.to_string());

        VlessConfig {
            uuid: self.uuid.clone(),
            server_addr: self.server_addr.clone(),
            server_name: self.server_name.clone(),
            reality_public_key: self.reality_public_key.clone(),
            reality_short_id: self.reality_short_id.clone(),
            flow: self.flow.clone(),
            address,
            netmask: None,
            dns: self.dns.clone(),
            mtu: self.mtu,
            allowed_ips: Some(self.allowed_ips.clone()),
        }
    }

    /// Get address as IpNetwork
    pub fn address_network(&self) -> Result<IpNetwork, ipnetwork::IpNetworkError> {
        IpNetwork::from_str(&self.address)
    }

    /// The resolvers only; see [`WgConfig::dns_servers`].
    pub fn dns_servers(&self) -> Vec<IpAddr> {
        dns_servers_of(self.dns.as_deref())
    }

    /// Get allowed IPs as `Vec<IpNetwork>`
    pub fn allowed_ips_networks(&self) -> Vec<IpNetwork> {
        self.allowed_ips
            .split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect()
    }

    /// Get MTU (default 1500 for VLESS)
    pub fn get_mtu(&self) -> u16 {
        self.mtu.unwrap_or(1500)
    }
}

/// AmneziaWG 2.0 obfuscation parameters, parsed from the `[Interface]` section of an
/// AmneziaWG `.conf`. Applied to the gotatun device via `.with_awg(...)`. `H1`–`H4` are
/// strings (single value or "lo-hi" range); `I1`–`I5` are CPS tag specs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwgObfuscation {
    pub jc: u32,
    pub jmin: u32,
    pub jmax: u32,
    pub s1: u32,
    pub s2: u32,
    pub s3: u32,
    pub s4: u32,
    pub h1: String,
    pub h2: String,
    pub h3: String,
    pub h4: String,
    pub i1: Option<String>,
    pub i2: Option<String>,
    pub i3: Option<String>,
    pub i4: Option<String>,
    pub i5: Option<String>,
}

impl Default for AwgObfuscation {
    /// Defaults to standard-WireGuard behaviour (no obfuscation); real values come from the
    /// server-issued config.
    fn default() -> Self {
        Self {
            jc: 0,
            jmin: 0,
            jmax: 0,
            s1: 0,
            s2: 0,
            s3: 0,
            s4: 0,
            h1: "1".into(),
            h2: "2".into(),
            h3: "3".into(),
            h4: "4".into(),
            i1: None,
            i2: None,
            i3: None,
            i4: None,
            i5: None,
        }
    }
}

/// AmneziaWG config: a WireGuard config plus interface-wide obfuscation. The tunnel runs
/// through the same gotatun device as WireGuard, with the obfuscation applied at build time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AwgConfig {
    pub wg: WgConfig,
    pub obfuscation: AwgObfuscation,
}

/// AmneziaWG `[Interface]` obfuscation keys, whose presence tells an AmneziaWG `.conf` from a
/// plain WireGuard one.
const AWG_OBF_KEYS: &[&str] = &[
    "jc", "jmin", "jmax", "s1", "s2", "s3", "s4", "h1", "h2", "h3", "h4", "i1", "i2", "i3", "i4",
    "i5",
];

impl Sections {
    fn is_amneziawg(&self) -> bool {
        self.interface
            .iter()
            .any(|e| AWG_OBF_KEYS.contains(&e.key.as_str()))
    }
}

impl AwgConfig {
    /// Parse an AmneziaWG `.conf` (WireGuard config + obfuscation params).
    pub fn from_config_str(config: &str) -> Result<Self, ConfigParseError> {
        Self::from_sections(&parse_sections(config)?)
    }

    fn from_sections(sections: &Sections) -> Result<Self, ConfigParseError> {
        let wg = WgConfig::from_sections(sections)?;
        let mut obf = AwgObfuscation::default();

        let number = |key: &str, slot: &mut u32| -> Result<(), ConfigParseError> {
            if let Some(entry) = sections.interface(key) {
                *slot = parsed::<u32>(entry)?;
            }
            Ok(())
        };
        number("jc", &mut obf.jc)?;
        number("jmin", &mut obf.jmin)?;
        number("jmax", &mut obf.jmax)?;
        number("s1", &mut obf.s1)?;
        number("s2", &mut obf.s2)?;
        number("s3", &mut obf.s3)?;
        number("s4", &mut obf.s4)?;

        let header = |key: &str, slot: &mut String| -> Result<(), ConfigParseError> {
            if let Some(entry) = sections.interface(key) {
                if entry.value.is_empty() {
                    return Err(invalid(entry, "a header spec cannot be empty"));
                }
                *slot = entry.value.clone();
            }
            Ok(())
        };
        header("h1", &mut obf.h1)?;
        header("h2", &mut obf.h2)?;
        header("h3", &mut obf.h3)?;
        header("h4", &mut obf.h4)?;

        let signature = |key: &str| -> Option<String> {
            sections
                .interface(key)
                .map(|e| e.value.clone())
                .filter(|v| !v.is_empty())
        };
        obf.i1 = signature("i1");
        obf.i2 = signature("i2");
        obf.i3 = signature("i3");
        obf.i4 = signature("i4");
        obf.i5 = signature("i5");

        Ok(Self {
            wg,
            obfuscation: obf,
        })
    }
}

/// Protocol-agnostic VPN configuration.
///
/// Each variant wraps a protocol-specific config. Common VPN concepts
/// (endpoint, address, DNS, etc.) are exposed via methods on this enum,
/// so the connect flow doesn't need to know which protocol is in use.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(tag = "protocol", content = "config")]
pub enum ProtocolConfig {
    #[serde(rename = "wireguard")]
    WireGuard(WgConfig),
    /// AmneziaWG — WireGuard + obfuscation. Runs through the same gotatun tunnel path.
    #[serde(rename = "amneziawg")]
    #[specta(skip)]
    AmneziaWg(AwgConfig),
    #[serde(rename = "vless")]
    #[specta(skip)]
    Vless(VlessVpnConfig),
}

impl ProtocolConfig {
    /// Read a config in whichever of the three forms it comes: a `vless://` URI, an AmneziaWG
    /// `.conf` (an `[Interface]` carrying obfuscation keys), or a plain WireGuard `.conf`.
    pub fn parse(raw: &str) -> Result<Self, ConfigParseError> {
        let trimmed = raw.trim();
        if trimmed.starts_with("vless://") {
            return VlessVpnConfig::from_uri(trimmed).map(Self::Vless);
        }
        let sections = parse_sections(raw)?;
        if sections.is_amneziawg() {
            AwgConfig::from_sections(&sections).map(Self::AmneziaWg)
        } else {
            WgConfig::from_sections(&sections).map(Self::WireGuard)
        }
    }

    /// Server endpoint as "host:port" string.
    pub fn endpoint_str(&self) -> &str {
        match self {
            Self::WireGuard(wg) => &wg.peer_endpoint,
            Self::AmneziaWg(awg) => &awg.wg.peer_endpoint,
            Self::Vless(vless) => &vless.server_addr,
        }
    }

    /// Local tunnel address string (e.g. "10.0.0.2/32").
    pub fn address(&self) -> &str {
        match self {
            Self::WireGuard(wg) => &wg.address,
            Self::AmneziaWg(awg) => &awg.wg.address,
            Self::Vless(vless) => &vless.address,
        }
    }

    /// Local tunnel address as IpNetwork.
    pub fn address_network(&self) -> Result<IpNetwork, ipnetwork::IpNetworkError> {
        match self {
            Self::WireGuard(wg) => wg.address_network(),
            Self::AmneziaWg(awg) => awg.wg.address_network(),
            Self::Vless(vless) => vless.address_network(),
        }
    }

    /// DNS servers.
    pub fn dns_servers(&self) -> Vec<IpAddr> {
        match self {
            Self::WireGuard(wg) => wg.dns_servers(),
            Self::AmneziaWg(awg) => awg.wg.dns_servers(),
            Self::Vless(vless) => vless.dns_servers(),
        }
    }

    /// Allowed IPs / routes.
    pub fn allowed_ips_networks(&self) -> Vec<IpNetwork> {
        match self {
            Self::WireGuard(wg) => wg.allowed_ips_networks(),
            Self::AmneziaWg(awg) => awg.wg.allowed_ips_networks(),
            Self::Vless(vless) => vless.allowed_ips_networks(),
        }
    }

    /// Tunnel MTU.
    pub fn get_mtu(&self) -> u16 {
        match self {
            Self::WireGuard(wg) => wg.get_mtu(),
            Self::AmneziaWg(awg) => awg.wg.get_mtu(),
            Self::Vless(vless) => vless.get_mtu(),
        }
    }

    /// Protocol name for display / persistence.
    pub fn protocol(&self) -> Protocol {
        match self {
            Self::WireGuard(_) => Protocol::WireGuard,
            Self::AmneziaWg(_) => Protocol::AmneziaWg,
            Self::Vless(_) => Protocol::Vless,
        }
    }
}

/// Multi-config storage: holds WG, AmneziaWG, and VLESS configs with an active selector.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SavedVpnConfigs {
    /// The protocol that last actually worked. `None` until something has connected.
    ///
    /// `serde(alias)` keeps reading the pre-enum `active_protocol` string already on disk and in
    /// OS keyrings; an unparseable legacy value migrates to `None` rather than silently becoming
    /// WireGuard (see [`Preference`]).
    #[serde(alias = "active_protocol", default)]
    pub preferred_protocol: Preference,
    /// Cached WireGuard config (if any)
    pub wireguard: Option<WgConfig>,
    /// Cached AmneziaWG config (if any)
    #[serde(default)]
    pub amneziawg: Option<AwgConfig>,
    /// Cached VLESS config (if any)
    #[serde(default)]
    pub vless: Option<VlessVpnConfig>,
}

impl SavedVpnConfigs {
    /// The cached config for one specific protocol, if it is stored.
    pub fn get(&self, protocol: Protocol) -> Option<ProtocolConfig> {
        match protocol {
            Protocol::WireGuard => self.wireguard.clone().map(ProtocolConfig::WireGuard),
            Protocol::AmneziaWg => self.amneziawg.clone().map(ProtocolConfig::AmneziaWg),
            Protocol::Vless => self.vless.clone().map(ProtocolConfig::Vless),
        }
    }

    /// Which protocols have a cached config. This is a SET: the order is [`Protocol::ALL`] for
    /// determinism, not a statement of preference.
    pub fn available_protocols(&self) -> Vec<Protocol> {
        Protocol::ALL
            .iter()
            .copied()
            .filter(|&p| self.get(p).is_some())
            .collect()
    }

    /// Whether any config is stored.
    pub fn has_any(&self) -> bool {
        self.wireguard.is_some() || self.amneziawg.is_some() || self.vless.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "aGVsbG93b3JsZGhlbGxvd29ybGRoZWxsb3dvcmxkMTI=";

    fn wg_conf(interface_extra: &str, peer_extra: &str) -> String {
        format!(
            "[Interface]\nPrivateKey = {KEY}\nAddress = 10.0.0.2/32\n{interface_extra}\n\
             [Peer]\nPublicKey = {KEY}\nEndpoint = vpn.example.com:51820\n{peer_extra}\n"
        )
    }

    fn err_of(raw: &str) -> ConfigParseError {
        ProtocolConfig::parse(raw).expect_err("must be rejected")
    }

    #[test]
    fn a_plain_wireguard_conf_parses_with_unknown_keys_tolerated() {
        let raw = wg_conf(
            "DNS = 1.1.1.1, 8.8.8.8\nMTU = 1380\nListenPort = 51820\nTable = off",
            "AllowedIPs = 0.0.0.0/0, ::/0\nPersistentKeepalive = 25",
        );
        match ProtocolConfig::parse(&raw).unwrap() {
            ProtocolConfig::WireGuard(wg) => {
                assert_eq!(wg.mtu, Some(1380));
                assert_eq!(wg.persistent_keepalive, Some(25));
                assert_eq!(wg.dns_servers().len(), 2);
                assert_eq!(wg.allowed_ips_networks().len(), 2);
            }
            other => panic!("expected WireGuard, got {other:?}"),
        }
    }

    #[test]
    fn search_domains_on_the_dns_line_are_kept_and_never_mistaken_for_resolvers() {
        // wg-quick reads a non-IP item as a search domain. The strict parser rejected the whole
        // file over one, and the previous lenient one dropped it on the floor.
        let raw = wg_conf("DNS = 1.1.1.1, corp.example, lan", "");
        match ProtocolConfig::parse(&raw).unwrap() {
            ProtocolConfig::WireGuard(wg) => {
                assert_eq!(wg.dns.as_deref(), Some("1.1.1.1, corp.example, lan"));
                assert_eq!(
                    wg.dns_servers(),
                    vec!["1.1.1.1".parse::<IpAddr>().unwrap()],
                    "only the resolver reaches the platform"
                );
                assert_eq!(wg.dns_search_domains(), vec!["corp.example", "lan"]);
            }
            other => panic!("expected WireGuard, got {other:?}"),
        }

        // Neither an address nor a hostname is still a typo, not a domain.
        for bad in [
            "1.1.1",
            "corp..example",
            "-corp.example",
            "corp example",
            "::1::",
        ] {
            let raw = wg_conf(&format!("DNS = 1.1.1.1, {bad}"), "");
            assert!(
                matches!(err_of(&raw), ConfigParseError::InvalidValue { key, .. } if key == "dns"),
                "`{bad}` must be rejected"
            );
        }
    }

    #[test]
    fn obfuscation_keys_make_it_an_amneziawg_conf() {
        let raw = wg_conf(
            "Jc = 4\nJmin = 40\nJmax = 70\nS1 = 15\nS2 = 18\nH1 = 5-10\nI1 = <b 0xf6>",
            "",
        );
        match ProtocolConfig::parse(&raw).unwrap() {
            ProtocolConfig::AmneziaWg(awg) => {
                assert_eq!(awg.obfuscation.jc, 4);
                assert_eq!(awg.obfuscation.jmin, 40);
                assert_eq!(awg.obfuscation.s2, 18);
                assert_eq!(awg.obfuscation.h1, "5-10");
                assert_eq!(
                    awg.obfuscation.h2, "2",
                    "unset headers keep the WireGuard default"
                );
                assert_eq!(awg.obfuscation.i1.as_deref(), Some("<b 0xf6>"));
            }
            other => panic!("expected AmneziaWG, got {other:?}"),
        }
    }

    #[test]
    fn a_typo_in_a_value_is_an_error_not_a_silently_dropped_entry() {
        // Each of these used to import fine and then quietly misbehave: the bad AllowedIPs
        // entry vanished from the routes, the bad DNS server from the resolver list, the bad Jc
        // became 0 and the bad MTU became the default.
        for (interface, peer, key) in [
            ("", "AllowedIPs = 0.0.0.0/0, ::/O", "allowedips"),
            ("DNS = 1.1.1.1, 1.1.1", "", "dns"),
            ("Jc = four", "", "jc"),
            ("MTU = big", "", "mtu"),
            (
                "Address = 10.0.0.2/32",
                "PersistentKeepalive = soon",
                "persistentkeepalive",
            ),
        ] {
            match err_of(&wg_conf(interface, peer)) {
                ConfigParseError::InvalidValue { key: k, .. } => assert_eq!(k, key),
                other => panic!("expected `{key}` rejected, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_key_that_is_not_32_bytes_is_rejected_at_import() {
        let raw = format!(
            "[Interface]\nPrivateKey = aGVsbG8=\nAddress = 10.0.0.2/32\n[Peer]\nPublicKey = {KEY}\nEndpoint = h:1\n"
        );
        assert!(matches!(
            err_of(&raw),
            ConfigParseError::InvalidValue { key, .. } if key == "privatekey"
        ));
    }

    #[test]
    fn shape_errors_name_the_line() {
        assert_eq!(
            err_of("PrivateKey = x\n"),
            ConfigParseError::OutsideSection {
                line: 1,
                key: "privatekey".into()
            }
        );
        assert_eq!(
            err_of("[Interface]\n\n# comment\nnonsense\n"),
            ConfigParseError::NotKeyValue {
                line: 4,
                text: "nonsense".into()
            }
        );
        assert_eq!(
            err_of("[Interface]\n[Extra]\n"),
            ConfigParseError::UnknownSection {
                line: 2,
                name: "Extra".into()
            }
        );
    }

    #[test]
    fn a_missing_required_key_names_its_section() {
        let raw = format!(
            "[Interface]\nPrivateKey = {KEY}\nAddress = 10.0.0.2/32\n[Peer]\nPublicKey = {KEY}\n"
        );
        assert_eq!(
            err_of(&raw),
            ConfigParseError::Missing {
                section: "Peer",
                key: "Endpoint"
            }
        );
    }

    #[test]
    fn an_endpoint_needs_a_port() {
        assert!(matches!(
            err_of(&wg_conf("", "Endpoint = vpn.example.com")),
            ConfigParseError::InvalidValue { key, .. } if key == "endpoint"
        ));
    }

    #[test]
    fn a_vless_uri_is_a_vless_config() {
        let raw = "vless://0f7f6d3c-0a1c-4f1e-9d3a-1b2c3d4e5f60@vpn.example.com:443?security=reality&sni=example.com&pbk=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA&sid=0123abcd";
        assert!(matches!(
            ProtocolConfig::parse(raw),
            Ok(ProtocolConfig::Vless(_))
        ));
        assert!(matches!(
            err_of("vless://nobody"),
            ConfigParseError::Vless(_)
        ));
    }
}
