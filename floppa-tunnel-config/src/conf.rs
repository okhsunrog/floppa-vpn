//! The WireGuard / AmneziaWG `.conf` parser.
//!
//! Strict about shape, lenient about vocabulary: unknown *keys* are tolerated — the format has
//! many this does not model (`Table`, `PreUp`, `FwMark`…) — unknown *sections* are not. Every value
//! that is later needed for real — addresses, DNS servers, allowed IPs, the MTU, the AmneziaWG
//! numbers, the keys — is checked here, at import, so a typo is an error the user sees instead of
//! an entry silently dropped by a `filter_map(Result::ok)` deep in the connect path.

use crate::awg::AwgObfuscation;
use crate::route;
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use ipnetwork::IpNetwork;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::str::FromStr;

/// The TUN MTU when the `.conf` does not set one (WireGuard's own default).
pub const WIREGUARD_MTU: u16 = 1420;

/// The keepalive interval, in seconds, when the `.conf` does not set one. Both clients sit behind
/// NAT often enough that "off" is never the right answer.
pub const DEFAULT_KEEPALIVE: u16 = 25;

/// Why a `.conf` could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigParseError {
    /// A line inside a section that is not `Key = Value`.
    #[error("line {line}: `{text}` is not `Key = Value`")]
    NotKeyValue {
        /// 1-based line number.
        line: usize,
        /// The offending line, trimmed.
        text: String,
    },
    /// A `Key = Value` line before the first section header.
    #[error("line {line}: `{key}` appears before any section")]
    OutsideSection {
        /// 1-based line number.
        line: usize,
        /// The key, lower-cased.
        key: String,
    },
    /// A section other than `[Interface]` or `[Peer]`.
    #[error("line {line}: unknown section [{name}]")]
    UnknownSection {
        /// 1-based line number.
        line: usize,
        /// The section name as written.
        name: String,
    },
    /// A required key is absent.
    #[error("[{section}] is missing `{key}`")]
    Missing {
        /// `Interface` or `Peer`.
        section: &'static str,
        /// The key in its canonical spelling.
        key: &'static str,
    },
    /// A value that does not parse as what its key requires.
    #[error("line {line}: `{key} = {value}`: {detail}")]
    InvalidValue {
        /// 1-based line number.
        line: usize,
        /// The key, lower-cased.
        key: String,
        /// The value as written.
        value: String,
        /// What was expected.
        detail: String,
    },
}

/// A WireGuard key that does not decode to 32 bytes.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    /// Not valid standard base64.
    #[error("not base64: {0}")]
    Base64(#[from] base64::DecodeError),
    /// Valid base64 of the wrong length.
    #[error("a key is 32 bytes, this one decodes to {0}")]
    Length(usize),
}

fn decode_key(encoded: &str) -> Result<[u8; 32], KeyError> {
    let bytes = BASE64.decode(encoded)?;
    let len = bytes.len();
    bytes.try_into().map_err(|_| KeyError::Length(len))
}

/// A private or preshared key. Its `Debug` output is redacted; the base64 form is only available
/// on purpose, through [`SecretKey::to_base64`].
#[derive(Clone, PartialEq, Eq)]
pub struct SecretKey([u8; 32]);

impl SecretKey {
    /// Wrap raw key bytes.
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The raw key bytes.
    pub const fn to_bytes(&self) -> [u8; 32] {
        self.0
    }

    /// The key as it appears in a `.conf`.
    pub fn to_base64(&self) -> String {
        BASE64.encode(self.0)
    }
}

impl FromStr for SecretKey {
    type Err = KeyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        decode_key(s).map(Self)
    }
}

impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretKey(<redacted>)")
    }
}

/// A peer's public key.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PublicKey([u8; 32]);

impl PublicKey {
    /// Wrap raw key bytes.
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The raw key bytes.
    pub const fn to_bytes(&self) -> [u8; 32] {
        self.0
    }

    /// The key as it appears in a `.conf`.
    pub fn to_base64(&self) -> String {
        BASE64.encode(self.0)
    }
}

impl FromStr for PublicKey {
    type Err = KeyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        decode_key(s).map(Self)
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_base64())
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey({})", self.to_base64())
    }
}

/// Why an `Endpoint =` value is not `host:port`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EndpointParseError {
    /// No `:port` at all.
    #[error("expected host:port")]
    MissingPort,
    /// Nothing before the `:`.
    #[error("expected host:port, the host is empty")]
    EmptyHost,
    /// The part after the last `:` is not a port number.
    #[error("expected host:port, `{0}` is not a port")]
    Port(String),
}

/// A peer endpoint as written in the `.conf`: a host name or address literal, and a port.
///
/// The host is kept as text — an IPv6 literal keeps its brackets — and resolved by the client at
/// connect time; [`Endpoint::ip`] is there for the case where it is already an address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// Host name or address literal, verbatim.
    pub host: String,
    /// UDP port.
    pub port: u16,
}

impl Endpoint {
    /// The host as an address, if it is an address literal rather than a name.
    pub fn ip(&self) -> Option<IpAddr> {
        let host = self
            .host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .unwrap_or(&self.host);
        host.parse().ok()
    }

    /// The endpoint as a socket address, if the host is an address literal.
    pub fn socket_addr(&self) -> Option<SocketAddr> {
        self.ip().map(|ip| SocketAddr::new(ip, self.port))
    }
}

impl FromStr for Endpoint {
    type Err = EndpointParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (host, port) = s.rsplit_once(':').ok_or(EndpointParseError::MissingPort)?;
        if host.is_empty() {
            return Err(EndpointParseError::EmptyHost);
        }
        let port = port
            .parse()
            .map_err(|_| EndpointParseError::Port(port.to_string()))?;
        Ok(Self {
            host: host.to_string(),
            port,
        })
    }
}

impl From<SocketAddr> for Endpoint {
    fn from(addr: SocketAddr) -> Self {
        let host = match addr.ip() {
            IpAddr::V4(ip) => ip.to_string(),
            IpAddr::V6(ip) => format!("[{ip}]"),
        };
        Self {
            host,
            port: addr.port(),
        }
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.host, self.port)
    }
}

/// One item of a `DNS =` line, in wg-quick's reading of it: an IP address is a resolver, anything
/// else is a search domain.
///
/// Search domains are kept (a `.conf` written for wg-quick must import at all — the strict parser
/// used to reject the whole file over a `corp.example` it would then have ignored anyway) but the
/// clients do not apply them to the tunnel's resolver configuration yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DnsEntry {
    /// A resolver address.
    Server(IpAddr),
    /// A search domain.
    SearchDomain(String),
}

/// A `DNS =` item that is neither an address nor a plausible domain.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`{0}` is neither an IP address nor a search domain")]
pub struct DnsEntryError(String);

impl FromStr for DnsEntry {
    type Err = DnsEntryError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Ok(ip) = s.parse::<IpAddr>() {
            return Ok(Self::Server(ip));
        }
        if is_plausible_hostname(s) {
            return Ok(Self::SearchDomain(s.to_string()));
        }
        Err(DnsEntryError(s.to_string()))
    }
}

impl fmt::Display for DnsEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Server(ip) => ip.fmt(f),
            Self::SearchDomain(domain) => f.write_str(domain),
        }
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

/// Render items the way a `.conf` lists them: comma-separated with a space.
pub fn comma_list<I>(items: I) -> String
where
    I: IntoIterator,
    I::Item: fmt::Display,
{
    items
        .into_iter()
        .map(|item| item.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The `[Interface]` section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceConfig {
    /// `PrivateKey`.
    pub private_key: SecretKey,
    /// The first `Address`: the tunnel's own address, with its prefix.
    pub address: IpNetwork,
    /// Any further `Address` items (wg-quick allows a list; typically the IPv6 twin).
    pub extra_addresses: Vec<IpNetwork>,
    /// `DNS`, in order: resolvers and search domains.
    pub dns: Vec<DnsEntry>,
    /// `MTU`, when set. See [`TunnelConfig::mtu`] for the effective value.
    pub mtu: Option<u16>,
    /// `ListenPort`, when set. Clients do not need one; it is kept so the file round-trips.
    pub listen_port: Option<u16>,
}

/// The `[Peer]` section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerConfig {
    /// `PublicKey`.
    pub public_key: PublicKey,
    /// `PresharedKey`, when set.
    pub preshared_key: Option<SecretKey>,
    /// `Endpoint`. Required: a client config without one cannot connect anywhere.
    pub endpoint: Endpoint,
    /// `AllowedIPs`; the catch-all pair when the file has none.
    pub allowed_ips: Vec<IpNetwork>,
    /// `PersistentKeepalive`, when set. See [`TunnelConfig::keepalive`].
    pub persistent_keepalive: Option<u16>,
}

/// A parsed WireGuard or AmneziaWG client config.
///
/// The two are one type: an AmneziaWG `.conf` is a WireGuard `.conf` whose `[Interface]` carries
/// obfuscation keys, and both run through the same tunnel engine with [`Self::obfuscation`]
/// applied at build time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelConfig {
    /// The `[Interface]` section.
    pub interface: InterfaceConfig,
    /// The `[Peer]` section.
    pub peer: PeerConfig,
    /// `Some` for an AmneziaWG config, `None` for plain WireGuard.
    pub obfuscation: Option<AwgObfuscation>,
}

impl TunnelConfig {
    /// Parse a WireGuard or AmneziaWG `.conf`. Any AmneziaWG key in `[Interface]` makes it the
    /// latter; keys the file leaves out keep their WireGuard meaning
    /// ([`AwgObfuscation::wireguard`]).
    pub fn parse(text: &str) -> Result<Self, ConfigParseError> {
        Self::from_sections(&parse_sections(text)?)
    }

    /// Whether the config carries AmneziaWG obfuscation.
    pub fn is_amneziawg(&self) -> bool {
        self.obfuscation.is_some()
    }

    /// The TUN MTU: the file's, or [`WIREGUARD_MTU`].
    pub fn mtu(&self) -> u16 {
        self.interface.mtu.unwrap_or(WIREGUARD_MTU)
    }

    /// The keepalive interval in seconds: the file's, or [`DEFAULT_KEEPALIVE`].
    pub fn keepalive(&self) -> u16 {
        self.peer.persistent_keepalive.unwrap_or(DEFAULT_KEEPALIVE)
    }

    /// The resolvers on the `DNS` line, in order.
    pub fn dns_servers(&self) -> Vec<IpAddr> {
        self.interface
            .dns
            .iter()
            .filter_map(|entry| match entry {
                DnsEntry::Server(ip) => Some(*ip),
                DnsEntry::SearchDomain(_) => None,
            })
            .collect()
    }

    /// The search domains on the `DNS` line, in order.
    pub fn dns_search_domains(&self) -> Vec<&str> {
        self.interface
            .dns
            .iter()
            .filter_map(|entry| match entry {
                DnsEntry::SearchDomain(domain) => Some(domain.as_str()),
                DnsEntry::Server(_) => None,
            })
            .collect()
    }

    /// The `DNS` line as text, `None` when the file has none.
    pub fn dns_line(&self) -> Option<String> {
        (!self.interface.dns.is_empty()).then(|| comma_list(&self.interface.dns))
    }

    /// The `AllowedIPs` line as text.
    pub fn allowed_ips_line(&self) -> String {
        comma_list(&self.peer.allowed_ips)
    }

    fn from_sections(sections: &Sections) -> Result<Self, ConfigParseError> {
        let mut addresses = parsed_list::<IpNetwork>(required(
            sections.interface("address"),
            "Interface",
            "Address",
        )?)?
        .into_iter();
        // `split` always yields an item, so a parsed list is never empty; this is not a panic path.
        let address = addresses.next().ok_or(ConfigParseError::Missing {
            section: "Interface",
            key: "Address",
        })?;

        let interface = InterfaceConfig {
            private_key: parsed(required(
                sections.interface("privatekey"),
                "Interface",
                "PrivateKey",
            )?)?,
            address,
            extra_addresses: addresses.collect(),
            dns: sections
                .interface("dns")
                .map(parsed_list::<DnsEntry>)
                .transpose()?
                .unwrap_or_default(),
            mtu: sections.interface("mtu").map(parsed).transpose()?,
            listen_port: sections.interface("listenport").map(parsed).transpose()?,
        };

        let peer = PeerConfig {
            public_key: parsed(required(sections.peer("publickey"), "Peer", "PublicKey")?)?,
            preshared_key: sections.peer("presharedkey").map(parsed).transpose()?,
            endpoint: parsed(required(sections.peer("endpoint"), "Peer", "Endpoint")?)?,
            allowed_ips: match sections.peer("allowedips") {
                Some(entry) => parsed_list(entry)?,
                None => route::CATCH_ALL.to_vec(),
            },
            persistent_keepalive: sections
                .peer("persistentkeepalive")
                .map(parsed)
                .transpose()?,
        };

        let obfuscation = sections
            .is_amneziawg()
            .then(|| parse_obfuscation(sections))
            .transpose()?;

        Ok(Self {
            interface,
            peer,
            obfuscation,
        })
    }
}

/// One `Key = Value` line, with the key lower-cased for matching and the line kept for errors.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    line: usize,
    key: String,
    value: String,
}

/// The two sections a WireGuard-family `.conf` may contain.
#[derive(Debug, Default, PartialEq, Eq)]
struct Sections {
    interface: Vec<Entry>,
    peer: Vec<Entry>,
}

/// AmneziaWG `[Interface]` obfuscation keys, whose presence tells an AmneziaWG `.conf` from a
/// plain WireGuard one.
const AWG_OBF_KEYS: &[&str] = &[
    "jc", "jmin", "jmax", "s1", "s2", "s3", "s4", "h1", "h2", "h3", "h4", "i1", "i2", "i3", "i4",
    "i5",
];

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

    fn is_amneziawg(&self) -> bool {
        self.interface
            .iter()
            .any(|e| AWG_OBF_KEYS.contains(&e.key.as_str()))
    }
}

/// The one pass over the text.
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

fn parse_obfuscation(sections: &Sections) -> Result<AwgObfuscation, ConfigParseError> {
    let mut obf = AwgObfuscation::wireguard();

    for (key, slot) in [
        ("jc", &mut obf.jc),
        ("jmin", &mut obf.jmin),
        ("jmax", &mut obf.jmax),
        ("s1", &mut obf.s1),
        ("s2", &mut obf.s2),
        ("s3", &mut obf.s3),
        ("s4", &mut obf.s4),
    ] {
        if let Some(entry) = sections.interface(key) {
            *slot = parsed(entry)?;
        }
    }

    for (key, slot) in [
        ("h1", &mut obf.h1),
        ("h2", &mut obf.h2),
        ("h3", &mut obf.h3),
        ("h4", &mut obf.h4),
    ] {
        if let Some(entry) = sections.interface(key) {
            if entry.value.is_empty() {
                return Err(invalid(entry, "a header spec cannot be empty"));
            }
            *slot = entry.value.clone();
        }
    }

    for (key, slot) in [
        ("i1", &mut obf.i1),
        ("i2", &mut obf.i2),
        ("i3", &mut obf.i3),
        ("i4", &mut obf.i4),
        ("i5", &mut obf.i5),
    ] {
        if let Some(entry) = sections.interface(key) {
            *slot = entry.value.clone();
        }
    }

    Ok(obf)
}

/// Parse an entry's value as `T`.
fn parsed<T: FromStr>(entry: &Entry) -> Result<T, ConfigParseError>
where
    T::Err: fmt::Display,
{
    entry.value.parse::<T>().map_err(|e| invalid(entry, e))
}

/// Parse every comma-separated item of an entry's value as `T`. An empty value is one empty
/// item, and so an error: a key with nothing after the `=` is a mistake, not an absence.
fn parsed_list<T: FromStr>(entry: &Entry) -> Result<Vec<T>, ConfigParseError>
where
    T::Err: fmt::Display,
{
    entry
        .value
        .split(',')
        .map(|item| item.trim().parse::<T>().map_err(|e| invalid(entry, e)))
        .collect()
}

fn invalid(entry: &Entry, detail: impl fmt::Display) -> ConfigParseError {
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
        TunnelConfig::parse(raw).expect_err("must be rejected")
    }

    #[test]
    fn a_plain_wireguard_conf_parses_with_unknown_keys_tolerated() {
        let raw = wg_conf(
            "DNS = 1.1.1.1, 8.8.8.8\nMTU = 1380\nListenPort = 51820\nTable = off",
            "AllowedIPs = 0.0.0.0/0, ::/0\nPersistentKeepalive = 25",
        );
        let config = TunnelConfig::parse(&raw).unwrap();
        assert!(!config.is_amneziawg());
        assert_eq!(config.interface.mtu, Some(1380));
        assert_eq!(config.mtu(), 1380);
        assert_eq!(config.interface.listen_port, Some(51820));
        assert_eq!(config.peer.persistent_keepalive, Some(25));
        assert_eq!(config.dns_servers().len(), 2);
        assert_eq!(config.peer.allowed_ips.len(), 2);
        assert_eq!(config.interface.private_key.to_base64(), KEY);
        assert_eq!(config.peer.public_key.to_string(), KEY);
        assert_eq!(
            config.peer.endpoint,
            Endpoint {
                host: "vpn.example.com".into(),
                port: 51820
            }
        );
    }

    #[test]
    fn defaults_fill_in_what_the_file_leaves_out() {
        let config = TunnelConfig::parse(&wg_conf("", "")).unwrap();
        assert_eq!(config.mtu(), WIREGUARD_MTU);
        assert_eq!(config.keepalive(), DEFAULT_KEEPALIVE);
        assert_eq!(config.peer.allowed_ips, route::CATCH_ALL);
        assert_eq!(config.allowed_ips_line(), "0.0.0.0/0, ::/0");
        assert_eq!(config.dns_line(), None);
        assert!(config.interface.dns.is_empty());
        assert!(config.interface.extra_addresses.is_empty());
    }

    #[test]
    fn a_repeated_key_overrides_as_wg_setconf_does() {
        let config = TunnelConfig::parse(&wg_conf("MTU = 1280\nMTU = 1300", "")).unwrap();
        assert_eq!(config.interface.mtu, Some(1300));
    }

    #[test]
    fn an_address_list_keeps_the_first_as_the_tunnel_address() {
        let raw = wg_conf("Address = 10.0.0.2/32, fd00::2/128", "");
        let config = TunnelConfig::parse(&raw).unwrap();
        assert_eq!(config.interface.address, "10.0.0.2/32".parse().unwrap());
        assert_eq!(
            config.interface.extra_addresses,
            vec!["fd00::2/128".parse::<IpNetwork>().unwrap()]
        );
    }

    #[test]
    fn search_domains_on_the_dns_line_are_kept_and_never_mistaken_for_resolvers() {
        // wg-quick reads a non-IP item as a search domain. The strict parser rejected the whole
        // file over one, and the previous lenient one dropped it on the floor.
        let raw = wg_conf("DNS = 1.1.1.1, corp.example, lan", "");
        let config = TunnelConfig::parse(&raw).unwrap();
        assert_eq!(
            config.dns_line().as_deref(),
            Some("1.1.1.1, corp.example, lan")
        );
        assert_eq!(
            config.dns_servers(),
            vec!["1.1.1.1".parse::<IpAddr>().unwrap()],
            "only the resolver reaches the platform"
        );
        assert_eq!(config.dns_search_domains(), vec!["corp.example", "lan"]);

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
        let config = TunnelConfig::parse(&raw).unwrap();
        assert!(config.is_amneziawg());
        let obf = config.obfuscation.unwrap();
        assert_eq!(obf.jc, 4);
        assert_eq!(obf.jmin, 40);
        assert_eq!(obf.s2, 18);
        assert_eq!(obf.h1, "5-10");
        assert_eq!(obf.h2, "2", "unset headers keep the WireGuard default");
        assert_eq!(obf.s3, 0, "unset padding keeps the WireGuard default");
        assert_eq!(
            obf.signature_packets(),
            [Some("<b 0xf6>"), None, None, None, None]
        );
    }

    #[test]
    fn an_empty_header_spec_is_rejected() {
        assert!(matches!(
            err_of(&wg_conf("Jc = 4\nH1 =", "")),
            ConfigParseError::InvalidValue { key, .. } if key == "h1"
        ));
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
            ("ListenPort = 70000", "", "listenport"),
            ("Address = 10.0.0.2/33", "", "address"),
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
        assert!(matches!(
            err_of(&wg_conf("", "PresharedKey = not base64!")),
            ConfigParseError::InvalidValue { key, .. } if key == "presharedkey"
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
    fn an_endpoint_needs_a_host_and_a_port() {
        assert!(matches!(
            err_of(&wg_conf("", "Endpoint = vpn.example.com")),
            ConfigParseError::InvalidValue { key, .. } if key == "endpoint"
        ));
        assert_eq!(
            "vpn.example.com".parse::<Endpoint>(),
            Err(EndpointParseError::MissingPort)
        );
        assert_eq!(
            ":51820".parse::<Endpoint>(),
            Err(EndpointParseError::EmptyHost)
        );
        assert_eq!(
            "h:port".parse::<Endpoint>(),
            Err(EndpointParseError::Port("port".into()))
        );
    }

    #[test]
    fn an_endpoint_literal_round_trips_and_knows_its_address() {
        let named: Endpoint = "vpn.example.com:51820".parse().unwrap();
        assert_eq!(named.ip(), None);
        assert_eq!(named.to_string(), "vpn.example.com:51820");

        let v4: Endpoint = "1.2.3.4:51820".parse().unwrap();
        assert_eq!(v4.socket_addr(), Some("1.2.3.4:51820".parse().unwrap()));

        let v6: Endpoint = "[2001:db8::1]:51820".parse().unwrap();
        assert_eq!(v6.host, "[2001:db8::1]");
        assert_eq!(
            v6.socket_addr(),
            Some("[2001:db8::1]:51820".parse().unwrap())
        );
        assert_eq!(
            Endpoint::from("[2001:db8::1]:51820".parse::<SocketAddr>().unwrap()),
            v6
        );
    }

    #[test]
    fn keys_encode_back_to_their_conf_form_and_the_secret_does_not_leak_through_debug() {
        let secret: SecretKey = KEY.parse().unwrap();
        assert_eq!(secret.to_base64(), KEY);
        assert_eq!(format!("{secret:?}"), "SecretKey(<redacted>)");
        assert_eq!("aGVsbG8=".parse::<SecretKey>(), Err(KeyError::Length(5)));
        assert!(matches!(
            "not base64!".parse::<PublicKey>(),
            Err(KeyError::Base64(_))
        ));
        let public: PublicKey = KEY.parse().unwrap();
        assert_eq!(format!("{public:?}"), format!("PublicKey({KEY})"));
    }
}
