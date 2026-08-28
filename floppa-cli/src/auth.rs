use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

use floppa_api_client::{ApiClient, DeviceIdentity};

/// The uid/gid of the user behind `sudo`, so files created in their home stay theirs.
struct SudoUser {
    home: PathBuf,
    uid: u32,
    gid: u32,
}

/// `connect` needs root, so the CLI usually runs under sudo, where HOME and the config dir point
/// at root's. The token was saved by the invoking user: look there instead.
fn sudo_user() -> Option<SudoUser> {
    let user = std::env::var("SUDO_USER").ok()?;
    let uid: u32 = std::env::var("SUDO_UID").ok()?.parse().ok()?;
    let gid: u32 = std::env::var("SUDO_GID").ok()?.parse().ok()?;
    if uid == 0 {
        return None;
    }
    let passwd = fs::read_to_string("/etc/passwd").ok()?;
    let home = passwd.lines().find_map(|line| {
        let mut fields = line.split(':');
        (fields.next()? == user).then(|| fields.nth(4).map(PathBuf::from))?
    })?;
    Some(SudoUser { home, uid, gid })
}

pub fn config_dir() -> Result<PathBuf> {
    let sudo = sudo_user();
    let base = match &sudo {
        Some(sudo) => sudo.home.join(".config"),
        None => dirs::config_dir().ok_or_else(|| anyhow!("Cannot determine config directory"))?,
    };
    let dir = base.join("floppa-cli");
    if !dir.is_dir() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .with_context(|| format!("Failed to create {}", dir.display()))?;
        give_to_sudo_user(&dir, sudo.as_ref());
    }
    Ok(dir)
}

/// Files created as root in the invoking user's home are handed back to that user, so a later
/// unprivileged `login` can replace them.
fn give_to_sudo_user(path: &Path, sudo: Option<&SudoUser>) {
    if let Some(sudo) = sudo
        && let Err(e) = std::os::unix::fs::chown(path, Some(sudo.uid), Some(sudo.gid))
    {
        eprintln!("Could not chown {} to the sudo user: {e}", path.display());
    }
}

/// Write `content` to `path` with mode 0600 from the first byte: a temp file in the same
/// directory, then an atomic rename over the destination.
fn write_private(path: &Path, content: &str) -> Result<()> {
    let tmp = path.with_extension("tmp");
    let _ = fs::remove_file(&tmp);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .with_context(|| format!("Failed to create {}", tmp.display()))?;
    file.write_all(content.as_bytes())?;
    file.sync_all()?;
    drop(file);
    give_to_sudo_user(&tmp, sudo_user().as_ref());
    fs::rename(&tmp, path).with_context(|| format!("Failed to write {}", path.display()))
}

fn token_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("token"))
}

/// Where the login token comes from: `FLOPPA_TOKEN` inline, or a file (`--token-file` /
/// `FLOPPA_TOKEN_FILE`, default `<config dir>/floppa-cli/token`).
pub struct TokenSource {
    inline: Option<String>,
    file: Option<PathBuf>,
}

impl TokenSource {
    pub fn new(inline: Option<String>, file: Option<PathBuf>) -> Self {
        Self { inline, file }
    }

    fn path(&self) -> Result<PathBuf> {
        match &self.file {
            Some(path) => Ok(path.clone()),
            None => token_path(),
        }
    }

    pub fn load(&self) -> Result<Option<String>> {
        if let Some(token) = self.inline.as_deref().map(str::trim)
            && !token.is_empty()
        {
            return Ok(Some(token.to_string()));
        }
        let path = self.path()?;
        if !path.exists() {
            return Ok(None);
        }
        let token = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read token file {}", path.display()))?
            .trim()
            .to_string();
        Ok((!token.is_empty()).then_some(token))
    }

    /// The token, or the error the user needs to see.
    pub fn require(&self) -> Result<String> {
        self.load()?
            .context("Not logged in. Run `floppa-cli login` first.")
    }

    fn save(&self, token: &str) -> Result<()> {
        let path = self.path()?;
        if let Some(parent) = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty() && !p.is_dir())
        {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)
                .with_context(|| format!("Failed to create {}", parent.display()))?;
        }
        write_private(&path, token)
    }

    pub fn remove(&self) -> Result<()> {
        let path = self.path()?;
        if path.exists() {
            fs::remove_file(&path)?;
        }
        Ok(())
    }
}

/// The session (`jti`) a login token belongs to, read from its payload without verifying the
/// signature — the CLI only needs the id to ask the server to end that session; the server
/// verifies the token itself. `None` for a malformed token or one issued before sessions.
pub fn session_id(token: &str) -> Option<uuid::Uuid> {
    let payload = token.split('.').nth(1)?;
    let json = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&json).ok()?;
    claims.get("jti")?.as_str()?.parse().ok()
}

/// The device identity of this CLI installation. The id is generated once and persisted in the
/// config dir, so every run finds its own peer instead of adopting another device's.
pub fn device_identity() -> Result<DeviceIdentity> {
    let path = config_dir()?.join("device_id");
    let id = match fs::read_to_string(&path) {
        Ok(existing) if uuid::Uuid::parse_str(existing.trim()).is_ok() => {
            existing.trim().to_string()
        }
        _ => {
            let id = uuid::Uuid::new_v4().to_string();
            write_private(&path, &format!("{id}\n"))
                .with_context(|| format!("Failed to save device id to {}", path.display()))?;
            id
        }
    };
    Ok(DeviceIdentity {
        device_id: id,
        device_name: Some(hostname()),
        platform: std::env::consts::OS.to_string(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
    })
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_string()))
        .unwrap_or_else(|_| "floppa-cli".to_string())
}

/// Run the login flow: start local server, open browser, capture code, exchange for JWT.
pub async fn login(api_url: &str, tokens: &TokenSource) -> Result<()> {
    // Bind to a random port on 127.0.0.1
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    // The nonce rides along in the redirect URI; the server appends `&code=` to it. A callback
    // without the matching state is some other local process guessing our port.
    let state = uuid::Uuid::new_v4().simple().to_string();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback?state={state}");

    let auth_url = Url::parse_with_params(
        &format!("{}/auth/telegram/start", api_url.trim_end_matches('/')),
        [("redirect_uri", redirect_uri.as_str())],
    )
    .context("Invalid API URL")?;

    eprintln!("Opening browser for Telegram login...");
    eprintln!("If it doesn't open, visit: {auth_url}");

    if open::that(auth_url.as_str()).is_err() {
        eprintln!("Failed to open browser automatically.");
    }

    let code = tokio::time::timeout(LOGIN_TIMEOUT, wait_for_callback(listener, &state))
        .await
        .map_err(|_| {
            anyhow!(
                "Login timed out after {} minutes",
                LOGIN_TIMEOUT.as_secs() / 60
            )
        })??;

    // Exchange code for JWT
    let auth = match ApiClient::exchange_login_code(api_url, &code).await {
        Err(e) if e.is_unauthorized() => bail!("Login code expired or invalid. Try again."),
        other => other?,
    };
    tokens.save(&auth.token)?;

    let name = auth
        .user
        .username
        .as_deref()
        .or(auth.user.first_name.as_deref())
        .unwrap_or("user");

    eprintln!("Logged in as {name} (id: {})", auth.user.id);

    Ok(())
}

const LOGIN_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Serve the loopback listener until a `GET /callback?state=<expected>&code=...` arrives; any
/// other request (favicon, wrong path, wrong state) gets an error page and the wait continues.
async fn wait_for_callback(listener: TcpListener, expected_state: &str) -> Result<String> {
    loop {
        let (mut stream, _) = listener
            .accept()
            .await
            .context("Failed to accept callback connection")?;

        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await?;
        let request = String::from_utf8_lossy(&buf[..n]);
        let path = request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or("/");

        let (status, message) = match parse_callback(path, expected_state) {
            Ok(code) => {
                respond(
                    &mut stream,
                    "200 OK",
                    "Login successful!",
                    "You can close this tab and return to the terminal.",
                )
                .await?;
                return Ok(code);
            }
            Err(CallbackError::NotCallback) => ("404 Not Found", "Not found"),
            Err(CallbackError::BadState) => ("400 Bad Request", "Login state mismatch"),
            Err(CallbackError::MissingCode) => ("400 Bad Request", "Missing login code"),
        };
        eprintln!("Ignoring request {path}: {message}");
        respond(
            &mut stream,
            status,
            message,
            "Return to the terminal and try again.",
        )
        .await?;
    }
}

enum CallbackError {
    NotCallback,
    BadState,
    MissingCode,
}

/// Undo HTML entity escaping of the query separator.
///
/// The redirect back to this listener arrives with `&amp;` where it should have `&`, and the
/// login then fails every time: `url::query_pairs` reads `&amp;code=…` as a parameter *named*
/// `amp;code`, so the code is simply not there. The URL is built correctly server-side and put
/// into the page both as an `href` attribute and as a JS string literal, each escaped for its own
/// context — a browser decodes either back to `&`. Something between those two and this socket
/// does not, and rather than guess which, this repairs the damage wherever it happened.
///
/// Safe to do unconditionally: a literal `&amp;` is not a thing a real callback can contain. The
/// state nonce is hex and the login code is a base64url token, so neither carries `&` at all, let
/// alone the escaped form of it.
fn unescape_query_separators(path: &str) -> std::borrow::Cow<'_, str> {
    if path.contains("&amp;") {
        std::borrow::Cow::Owned(path.replace("&amp;", "&"))
    } else {
        std::borrow::Cow::Borrowed(path)
    }
}

/// Extract the login code from a request path, requiring the state nonce to match.
fn parse_callback(path: &str, expected_state: &str) -> Result<String, CallbackError> {
    let path = unescape_query_separators(path);
    let url = Url::parse("http://127.0.0.1")
        .and_then(|base| base.join(&path))
        .map_err(|_| CallbackError::NotCallback)?;
    if url.path() != "/callback" {
        return Err(CallbackError::NotCallback);
    }
    let mut code = None;
    let mut state = None;
    for (key, value) in url.query_pairs() {
        match &*key {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            _ => {}
        }
    }
    if state.as_deref() != Some(expected_state) {
        return Err(CallbackError::BadState);
    }
    code.filter(|c| !c.is_empty())
        .ok_or(CallbackError::MissingCode)
}

async fn respond(
    stream: &mut tokio::net::TcpStream,
    status: &str,
    title: &str,
    detail: &str,
) -> Result<()> {
    let body = format!(
        r#"<!DOCTYPE html>
<html><head><title>Floppa VPN</title></head>
<body style="font-family:system-ui;display:flex;justify-content:center;align-items:center;height:100vh;margin:0">
<div style="text-align:center">
<h1>{title}</h1>
<p>{detail}</p>
</div></body></html>"#
    );
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len(),
    );
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn session_id_comes_from_the_unverified_payload() {
        let jwt = |claims: &str| {
            format!(
                "eyJhbGciOiJIUzI1NiJ9.{}.sig",
                URL_SAFE_NO_PAD.encode(claims.as_bytes())
            )
        };
        let id = "0f3a1c2e-9b7d-4e6a-8c1f-2d3e4f5a6b7c";
        assert_eq!(
            session_id(&jwt(&format!(r#"{{"sub":1,"jti":"{id}"}}"#))),
            Some(id.parse().unwrap())
        );
        // Legacy token (no jti), a non-UUID jti, and garbage.
        assert_eq!(session_id(&jwt(r#"{"sub":1}"#)), None);
        assert_eq!(session_id(&jwt(r#"{"jti":"nope"}"#)), None);
        assert_eq!(session_id("not.a.jwt"), None);
        assert_eq!(session_id(""), None);
    }

    #[test]
    fn callback_requires_path_state_and_code() {
        let ok = parse_callback("/callback?state=abc&code=x%2Fy", "abc");
        assert_eq!(ok.ok().as_deref(), Some("x/y"));
        assert!(matches!(
            parse_callback("/favicon.ico", "abc"),
            Err(CallbackError::NotCallback)
        ));
        assert!(matches!(
            parse_callback("/callback?state=other&code=x", "abc"),
            Err(CallbackError::BadState)
        ));
        assert!(matches!(
            parse_callback("/callback?code=x", "abc"),
            Err(CallbackError::BadState)
        ));
        assert!(matches!(
            parse_callback("/callback?state=abc", "abc"),
            Err(CallbackError::MissingCode)
        ));
    }

    #[test]
    fn a_callback_whose_separator_arrived_html_escaped_still_yields_its_code() {
        // Observed on every real login: the browser delivered
        // `/callback?state=…&amp;code=…`, and `query_pairs` then read the second parameter as one
        // *named* `amp;code` — so the code was missing and the login failed, every time, with no
        // hint as to why. The escaping is correct in both places the server writes the URL into
        // its page; what is not correct is what reaches this socket.
        let ok = parse_callback("/callback?state=abc&amp;code=x%2Fy", "abc");
        assert_eq!(ok.ok().as_deref(), Some("x/y"));

        // The repair is not a licence to accept a mismatched state.
        assert!(matches!(
            parse_callback("/callback?state=other&amp;code=x", "abc"),
            Err(CallbackError::BadState)
        ));

        // An ordinary callback is untouched — including a code whose own bytes are percent-encoded.
        let plain = parse_callback("/callback?state=abc&code=a%2Bb", "abc");
        assert_eq!(plain.ok().as_deref(), Some("a+b"));
    }

    #[test]
    fn write_private_creates_0600_and_replaces_atomically() {
        let dir = std::env::temp_dir().join(format!("floppa-cli-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("token");

        write_private(&path, "first").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "first");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        write_private(&path, "second").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "second");
        assert!(!path.with_extension("tmp").exists());

        fs::remove_dir_all(&dir).unwrap();
    }
}
