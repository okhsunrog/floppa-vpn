//! The slice of the server API that provisioning talks to, and the one HTTP client behind it.
//!
//! Narrow on purpose: every method here exists because [`super`] calls it, and each returns only
//! what that caller reads. The trait is what a test hands fakes to; [`HttpApi`] is the real thing.
//!
//! # The distinction the whole module rests on
//!
//! "This device has no peer" and "we could not find out" are different answers, and only a `404`
//! is the first one. That is why [`ProvisionApi::peer_by_device`] returns `Result<Option<_>, _>`
//! rather than folding a failure into `None`: reading a network failure as "no peer" is what makes
//! an offline start create a duplicate, and what makes a reconnect re-provision a peer that was
//! never gone.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use std::time::Duration;

use crate::vpn::protocol::Protocol;

/// How long any one call may take before it counts as no answer.
///
/// Generous next to a request that works, short next to a user waiting: everything here runs
/// while somebody is looking at a connect button, or while the actor is deciding whether a peer
/// still exists.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// The protocols backed by a per-device peer row. VLESS is per-user and has no peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum WgProtocol {
    Wireguard,
    Amneziawg,
}

impl WgProtocol {
    pub fn as_str(self) -> &'static str {
        match self {
            WgProtocol::Wireguard => "wireguard",
            WgProtocol::Amneziawg => "amneziawg",
        }
    }

    /// The other one. A device holds both when the server offers both, so this is how the
    /// secondary is named without repeating the pairing at every call site.
    pub fn other(self) -> Self {
        match self {
            WgProtocol::Wireguard => WgProtocol::Amneziawg,
            WgProtocol::Amneziawg => WgProtocol::Wireguard,
        }
    }
}

impl From<WgProtocol> for Protocol {
    fn from(p: WgProtocol) -> Self {
        match p {
            WgProtocol::Wireguard => Protocol::WireGuard,
            WgProtocol::Amneziawg => Protocol::AmneziaWg,
        }
    }
}

impl TryFrom<Protocol> for WgProtocol {
    type Error = ();

    fn try_from(p: Protocol) -> Result<Self, ()> {
        match p {
            Protocol::WireGuard => Ok(WgProtocol::Wireguard),
            Protocol::AmneziaWg => Ok(WgProtocol::Amneziawg),
            Protocol::Vless => Err(()),
        }
    }
}

/// The `error` field of the body the server returns for every non-2xx response.
///
/// The same set `ApiErrorCode` names in `floppa-web-shared/src/utils/apiError.ts`, so a refusal
/// reads the same on both sides of the boundary. Parsed rather than matched as a string: a
/// misspelling here is a compile error, which is the whole point of naming them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiErrorCode {
    BadGateway,
    BadRequest,
    Conflict,
    Forbidden,
    InternalError,
    NotFound,
    TooManyRequests,
    Unauthorized,
    AmneziawgNotConfigured,
    DatabaseError,
    InvalidCredentials,
    InvalidInstallation,
    InvalidLogin,
    InvalidPassword,
    LoginTaken,
    NoActiveSubscription,
    NoAvailableIps,
    PeerAlreadyExists,
    PeerLimitReached,
    VlessNotConfigured,
}

impl FromStr for ApiErrorCode {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        Ok(match s {
            "bad_gateway" => Self::BadGateway,
            "bad_request" => Self::BadRequest,
            "conflict" => Self::Conflict,
            "forbidden" => Self::Forbidden,
            "internal_error" => Self::InternalError,
            "not_found" => Self::NotFound,
            "too_many_requests" => Self::TooManyRequests,
            "unauthorized" => Self::Unauthorized,
            "amneziawg_not_configured" => Self::AmneziawgNotConfigured,
            "database_error" => Self::DatabaseError,
            "invalid_credentials" => Self::InvalidCredentials,
            "invalid_installation" => Self::InvalidInstallation,
            "invalid_login" => Self::InvalidLogin,
            "invalid_password" => Self::InvalidPassword,
            "login_taken" => Self::LoginTaken,
            "no_active_subscription" => Self::NoActiveSubscription,
            "no_available_ips" => Self::NoAvailableIps,
            "peer_already_exists" => Self::PeerAlreadyExists,
            "peer_limit_reached" => Self::PeerLimitReached,
            "vless_not_configured" => Self::VlessNotConfigured,
            _ => return Err(()),
        })
    }
}

/// A refusal the server actually sent, as opposed to a request that never got one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub status: u16,
    /// `None` when the server named a code this build does not know — a newer server, or a body
    /// that is not one of ours. The raw text is kept in [`Refusal::raw_code`] either way.
    pub code: Option<ApiErrorCode>,
    pub raw_code: String,
    pub message: String,
}

impl Refusal {
    pub fn is(&self, code: ApiErrorCode) -> bool {
        self.code == Some(code)
    }
}

/// Why a call did not produce what was asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiFailure {
    /// Nothing answered: no network, DNS, TLS, a timeout. Never a statement about the server's
    /// contents — that is what keeps "no peer" and "could not ask" apart.
    Unreachable(String),
    /// The server answered and refused.
    Refused(Refusal),
}

impl ApiFailure {
    pub fn refusal(&self) -> Option<&Refusal> {
        match self {
            ApiFailure::Refused(r) => Some(r),
            ApiFailure::Unreachable(_) => None,
        }
    }
}

impl std::fmt::Display for ApiFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiFailure::Unreachable(why) => write!(f, "no answer from the server: {why}"),
            ApiFailure::Refused(r) => write!(f, "HTTP {} {}: {}", r.status, r.raw_code, r.message),
        }
    }
}

// ---------------------------------------------------------------------------
// Bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct MeResponse {
    /// Present and non-null exactly when the user may create a peer.
    #[serde(default)]
    pub subscription: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PublicConfig {
    pub amneziawg_available: bool,
    #[serde(default)]
    pub vless_available: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MyPeer {
    pub id: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreatePeerRequest {
    pub device_id: String,
    pub device_name: Option<String>,
    pub protocol: WgProtocol,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreatePeerResponse {
    pub id: i64,
    pub config: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VlessConfigResponse {
    pub uri: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpsertInstallationRequest {
    pub device_id: String,
    pub device_name: Option<String>,
    pub platform: Option<String>,
    pub app_version: Option<String>,
}

// ---------------------------------------------------------------------------
// The trait, and the client behind it
// ---------------------------------------------------------------------------

#[async_trait]
pub trait ProvisionApi: Send + Sync {
    async fn me(&self) -> Result<MeResponse, ApiFailure>;

    async fn public_config(&self) -> Result<PublicConfig, ApiFailure>;

    /// `Ok(None)` means the server said there is no such peer — and nothing else does.
    async fn peer_by_device(
        &self,
        device_id: &str,
        protocol: WgProtocol,
    ) -> Result<Option<MyPeer>, ApiFailure>;

    async fn peer_config(&self, id: i64) -> Result<String, ApiFailure>;

    async fn create_peer(&self, req: &CreatePeerRequest) -> Result<CreatePeerResponse, ApiFailure>;

    async fn vless_config(&self) -> Result<VlessConfigResponse, ApiFailure>;

    async fn upsert_installation(&self, req: &UpsertInstallationRequest) -> Result<(), ApiFailure>;
}

/// The real client: one `reqwest` client, one base URL, one bearer token.
///
/// Built per sync rather than held, because the token can change between two of them and a client
/// that baked in a stale one would keep sending it until something rebuilt it.
pub struct HttpApi {
    client: reqwest::Client,
    /// Without a trailing slash, e.g. `https://host.example/api`.
    base: String,
    token: String,
}

impl HttpApi {
    pub fn new(base: &str, token: &str) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| format!("could not build an HTTP client: {e}"))?;
        Ok(Self {
            client,
            base: base.trim_end_matches('/').to_owned(),
            token: token.to_owned(),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// Send a prepared request and turn everything that is not a 2xx into an [`ApiFailure`].
    ///
    /// The body is read as text first: a refusal is JSON when the server wrote it and something
    /// else when a proxy did, and a failure to parse must not be reported as unreachable — the
    /// server plainly answered.
    async fn send(&self, req: reqwest::RequestBuilder) -> Result<String, ApiFailure> {
        let response = req
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| ApiFailure::Unreachable(e.to_string()))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| ApiFailure::Unreachable(format!("reading the body: {e}")))?;

        if status.is_success() {
            return Ok(body);
        }

        #[derive(Deserialize)]
        struct ErrorBody {
            error: String,
            message: String,
        }
        let (raw_code, message) = match serde_json::from_str::<ErrorBody>(&body) {
            Ok(parsed) => (parsed.error, parsed.message),
            // Not one of our bodies. The status is still the honest part of the answer.
            Err(_) => (String::new(), body.chars().take(200).collect()),
        };
        Err(ApiFailure::Refused(Refusal {
            status: status.as_u16(),
            code: ApiErrorCode::from_str(&raw_code).ok(),
            raw_code,
            message,
        }))
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, ApiFailure> {
        let body = self.send(self.client.get(self.url(path))).await?;
        parse(&body)
    }
}

/// A 2xx body that does not parse is the server's fault, not the network's — said as a refusal
/// with the status that actually arrived would be a lie, so it is reported as unreachable with
/// the reason spelled out. Either way the caller treats it as "could not find out", which is the
/// safe reading.
fn parse<T: serde::de::DeserializeOwned>(body: &str) -> Result<T, ApiFailure> {
    serde_json::from_str(body)
        .map_err(|e| ApiFailure::Unreachable(format!("the server's answer did not parse: {e}")))
}

#[async_trait]
impl ProvisionApi for HttpApi {
    async fn me(&self) -> Result<MeResponse, ApiFailure> {
        self.get_json("/me").await
    }

    async fn public_config(&self) -> Result<PublicConfig, ApiFailure> {
        self.get_json("/config").await
    }

    async fn peer_by_device(
        &self,
        device_id: &str,
        protocol: WgProtocol,
    ) -> Result<Option<MyPeer>, ApiFailure> {
        let request = self
            .client
            .get(self.url(&format!("/me/peers/by-device/{device_id}")))
            .query(&[("protocol", protocol.as_str())]);
        match self.send(request).await {
            Ok(body) => parse(&body).map(Some),
            // The one status that is an answer rather than a failure.
            Err(ApiFailure::Refused(r)) if r.status == 404 => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn peer_config(&self, id: i64) -> Result<String, ApiFailure> {
        // Plain text, not JSON: this endpoint serves the `.conf` file itself.
        self.send(self.client.get(self.url(&format!("/me/peers/{id}/config"))))
            .await
    }

    async fn create_peer(&self, req: &CreatePeerRequest) -> Result<CreatePeerResponse, ApiFailure> {
        let body = self
            .send(self.client.post(self.url("/me/peers")).json(req))
            .await?;
        parse(&body)
    }

    async fn vless_config(&self) -> Result<VlessConfigResponse, ApiFailure> {
        self.get_json("/me/vless-config").await
    }

    async fn upsert_installation(&self, req: &UpsertInstallationRequest) -> Result<(), ApiFailure> {
        self.send(self.client.post(self.url("/me/installations")).json(req))
            .await
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_this_build_does_not_know_is_none_rather_than_a_guess() {
        assert_eq!(
            ApiErrorCode::from_str("no_active_subscription"),
            Ok(ApiErrorCode::NoActiveSubscription)
        );
        assert_eq!(ApiErrorCode::from_str("something_new"), Err(()));
    }

    #[test]
    fn the_protocols_that_have_peers_pair_up() {
        assert_eq!(WgProtocol::Amneziawg.other(), WgProtocol::Wireguard);
        assert_eq!(WgProtocol::Wireguard.other(), WgProtocol::Amneziawg);
        assert_eq!(WgProtocol::try_from(Protocol::Vless), Err(()));
        assert_eq!(
            WgProtocol::try_from(Protocol::AmneziaWg),
            Ok(WgProtocol::Amneziawg)
        );
    }

    #[test]
    fn a_protocol_goes_on_the_wire_as_the_server_spells_it() {
        assert_eq!(
            serde_json::to_string(&WgProtocol::Amneziawg).unwrap(),
            "\"amneziawg\""
        );
    }
}
