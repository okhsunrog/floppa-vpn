//! One HTTP client for the server, over the generated types.
//!
//! # The distinction the provisioning rests on
//!
//! "This device has no peer" and "we could not find out" are different answers, and only a `404`
//! is the first one. [`ProvisionApi::peer_by_device`] returns `Result<Option<_>, _>` so the two
//! cannot be confused: reading a network failure as "no peer" is what makes an offline start
//! create a duplicate peer, and what makes a reconnect replace a peer that was never gone.
//!
//! # Failures are typed by the server's code, not by the status
//!
//! The status is a mapping the server chose; the `error` code in the body *is* the contract, and
//! it is what `floppa-web-shared/src/utils/apiError.ts` matches on too. Both are kept — a caller
//! that only wants to know "was this a refusal at all" reads the status.

use async_trait::async_trait;
use std::str::FromStr;
use std::time::Duration;

use crate::schema::{
    ApiError as ApiErrorBody, AuthResponse, CreatePeerRequest, CreatePeerResponse,
    InstallationResponse, MeResponse, MyPeer, MyPeersResponse, Protocol, PublicConfig,
    UpsertInstallationRequest, VlessConfigResponse,
};

/// The TLS configuration every request goes out on.
///
/// Built once, with the Mozilla root store compiled in, rather than left to `reqwest`'s default —
/// which is the *platform* verifier, and on Android that needs a JNI handshake with the system
/// trust store before it will do anything. Without it the first HTTPS request panics with
/// "Expect rustls-platform-verifier to be initialized", which is exactly what a background peer
/// repair did on a device: the tunnel had reconnected, the actor had named the dead peer, and the
/// process died reaching for the server.
///
/// Bundled roots are the right answer here anyway. This client talks to one server, whose
/// certificate chains to a public CA, and the alternative would have every platform disagree
/// about what it trusts — including a corporate middlebox, which for a VPN client is not a
/// feature.
fn tls_config() -> rustls::ClientConfig {
    static CONFIG: std::sync::OnceLock<rustls::ClientConfig> = std::sync::OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let roots = rustls::RootCertStore {
                roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
            };
            rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth()
        })
        .clone()
}

/// How long any one call may take before it counts as no answer.
///
/// Generous next to a request that works, short next to a user waiting: everything here runs
/// while somebody is looking at a connect button, or while a tunnel is deciding whether its peer
/// still exists.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// The `error` field of the body every failing endpoint returns.
///
/// The same set the TypeScript client names in `apiError.ts`. Parsed rather than compared as a
/// string, so a misspelling is a compile error — which is the whole point of naming them.
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
    /// The server's own words. For a 4xx these describe the problem and are worth showing; for a
    /// 5xx they are a fixed string, because the details are logged on the server and never sent.
    pub message: String,
}

impl Refusal {
    pub fn is(&self, code: ApiErrorCode) -> bool {
        self.code == Some(code)
    }

    /// Whether the token is the problem, whatever the server called it.
    pub fn is_unauthorized(&self) -> bool {
        self.status == 401 || self.is(ApiErrorCode::Unauthorized)
    }

    /// What a caller can put in front of a user: the server's words, or the status when it sent
    /// none.
    pub fn detail(&self) -> String {
        if self.message.is_empty() {
            format!("HTTP {}", self.status)
        } else {
            self.message.clone()
        }
    }
}

/// Why a call did not produce what was asked for.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApiFailure {
    /// Nothing answered: no network, DNS, TLS, a timeout, or a body that did not parse. Never a
    /// statement about the server's contents — that is what keeps "no peer" and "could not ask"
    /// apart.
    #[error("no answer from the server: {0}")]
    Unreachable(String),
    /// The server answered, and refused.
    #[error("HTTP {} {}: {}", .0.status, .0.raw_code, .0.message)]
    Refused(Refusal),
}

impl ApiFailure {
    pub fn refusal(&self) -> Option<&Refusal> {
        match self {
            ApiFailure::Refused(r) => Some(r),
            ApiFailure::Unreachable(_) => None,
        }
    }

    pub fn is_unauthorized(&self) -> bool {
        self.refusal().is_some_and(Refusal::is_unauthorized)
    }
}

/// What the provisioning logic needs of a server.
///
/// A trait so a test can hand it a fake; [`ApiClient`] is the real thing. Narrow on purpose:
/// every method is here because provisioning calls it.
#[async_trait]
pub trait ProvisionApi: Send + Sync {
    async fn me(&self) -> Result<MeResponse, ApiFailure>;

    async fn public_config(&self) -> Result<PublicConfig, ApiFailure>;

    /// `Ok(None)` means the server said there is no such peer — and nothing else does.
    async fn peer_by_device(
        &self,
        device_id: &str,
        protocol: Protocol,
    ) -> Result<Option<MyPeer>, ApiFailure>;

    /// The `.conf` file for a peer, as text.
    async fn peer_config(&self, id: i64) -> Result<String, ApiFailure>;

    async fn create_peer(&self, req: &CreatePeerRequest) -> Result<CreatePeerResponse, ApiFailure>;

    async fn vless_config(&self) -> Result<VlessConfigResponse, ApiFailure>;

    async fn upsert_installation(
        &self,
        req: &UpsertInstallationRequest,
    ) -> Result<InstallationResponse, ApiFailure>;
}

/// The real client: one `reqwest` client, one base URL, one bearer token.
#[derive(Clone)]
pub struct ApiClient {
    client: reqwest::Client,
    /// Without a trailing slash, e.g. `https://host.example/api`.
    base: String,
    token: String,
}

impl ApiClient {
    /// Build a client for `base` authenticating as `token`.
    ///
    /// Cheap enough to build per operation, which is how callers should use it: a token is
    /// rewritten on every sliding refresh, and a client that baked in an old one would keep
    /// sending it.
    pub fn new(base: &str, token: &str) -> Result<Self, ApiFailure> {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .use_preconfigured_tls(tls_config())
            .build()
            .map_err(|e| ApiFailure::Unreachable(format!("could not build an HTTP client: {e}")))?;
        Ok(Self {
            client,
            base: base.trim_end_matches('/').to_owned(),
            token: token.to_owned(),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// Send a prepared request, returning the body as text or a typed failure.
    ///
    /// The body is read as text first: a refusal is one of our JSON bodies when the server wrote
    /// it and something else when a proxy did, and a body that does not parse must not be reported
    /// as unreachable — the server plainly answered.
    async fn send(&self, req: reqwest::RequestBuilder) -> Result<String, ApiFailure> {
        let response = req
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| ApiFailure::Unreachable(e.to_string()))?;
        read(response).await
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, ApiFailure> {
        parse(&self.send(self.client.get(self.url(path))).await?)
    }

    /// Every peer this account has, over every device.
    ///
    /// The app asks about one device; the CLI lists them, because it is used to look at an
    /// account rather than to be one device on it.
    pub async fn list_peers(&self) -> Result<Vec<MyPeer>, ApiFailure> {
        let body: MyPeersResponse = self.get_json("/me/peers").await?;
        Ok(body.peers)
    }

    /// Exchange a one-time login code for a token. The one call made without one.
    pub async fn exchange_login_code(base: &str, code: &str) -> Result<AuthResponse, ApiFailure> {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .use_preconfigured_tls(tls_config())
            .build()
            .map_err(|e| ApiFailure::Unreachable(format!("could not build an HTTP client: {e}")))?;
        let url = format!("{}/auth/telegram/exchange-code", base.trim_end_matches('/'));
        let response = client
            .post(url)
            .json(&serde_json::json!({ "code": code }))
            .send()
            .await
            .map_err(|e| ApiFailure::Unreachable(e.to_string()))?;
        parse(&read(response).await?)
    }

    /// Revoke one session — the CLI's `logout`, which ends this device's token server-side rather
    /// than merely forgetting it.
    pub async fn delete_session(&self, session_id: uuid::Uuid) -> Result<(), ApiFailure> {
        self.send(
            self.client
                .delete(self.url(&format!("/me/sessions/{session_id}"))),
        )
        .await
        .map(|_| ())
    }
}

/// Turn a response into its body, or into the typed failure the status describes.
async fn read(response: reqwest::Response) -> Result<String, ApiFailure> {
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| ApiFailure::Unreachable(format!("reading the body: {e}")))?;

    if status.is_success() {
        return Ok(body);
    }

    let (raw_code, message) = match serde_json::from_str::<ApiErrorBody>(&body) {
        Ok(parsed) => (parsed.error, parsed.message),
        // Not one of our bodies — a proxy, or a plain-text error. The status is still honest, and
        // whatever text arrived is better than nothing, trimmed so a whole HTML page does not end
        // up in a log line.
        Err(_) => (String::new(), body.trim().chars().take(200).collect()),
    };
    Err(ApiFailure::Refused(Refusal {
        status: status.as_u16(),
        code: ApiErrorCode::from_str(&raw_code).ok(),
        raw_code,
        message,
    }))
}

/// A 2xx body that does not parse is reported as unreachable rather than as a refusal: calling it
/// a refusal would attribute to the server a decision it did not make. Either way the caller reads
/// it as "could not find out", which is the safe reading.
fn parse<T: serde::de::DeserializeOwned>(body: &str) -> Result<T, ApiFailure> {
    serde_json::from_str(body)
        .map_err(|e| ApiFailure::Unreachable(format!("the server's answer did not parse: {e}")))
}

#[async_trait]
impl ProvisionApi for ApiClient {
    async fn me(&self) -> Result<MeResponse, ApiFailure> {
        self.get_json("/me").await
    }

    async fn public_config(&self) -> Result<PublicConfig, ApiFailure> {
        self.get_json("/config").await
    }

    async fn peer_by_device(
        &self,
        device_id: &str,
        protocol: Protocol,
    ) -> Result<Option<MyPeer>, ApiFailure> {
        let request = self
            .client
            .get(self.url(&format!("/me/peers/by-device/{device_id}")))
            .query(&[("protocol", protocol.to_string())]);
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
        parse(
            &self
                .send(self.client.post(self.url("/me/peers")).json(req))
                .await?,
        )
    }

    async fn vless_config(&self) -> Result<VlessConfigResponse, ApiFailure> {
        self.get_json("/me/vless-config").await
    }

    async fn upsert_installation(
        &self,
        req: &UpsertInstallationRequest,
    ) -> Result<InstallationResponse, ApiFailure> {
        parse(
            &self
                .send(self.client.post(self.url("/me/installations")).json(req))
                .await?,
        )
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
    fn the_server_spells_a_protocol_the_way_the_query_needs_it() {
        assert_eq!(Protocol::Amneziawg.to_string(), "amneziawg");
        assert_eq!(Protocol::Wireguard.to_string(), "wireguard");
    }

    #[test]
    fn a_refusal_with_nothing_to_say_still_says_something() {
        let bare = Refusal {
            status: 502,
            code: None,
            raw_code: String::new(),
            message: String::new(),
        };
        assert_eq!(bare.detail(), "HTTP 502");
    }
}
