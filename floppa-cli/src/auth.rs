use anyhow::{Context, Result, anyhow, bail};
use clap::ValueEnum;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::api::ApiClient;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LoginMethod {
    /// Open browser and authenticate through Telegram.
    Telegram,
    /// Authenticate with Floppa account login + password.
    Account,
}

fn token_path() -> Result<PathBuf> {
    Ok(crate::paths::floppa_config_dir()?.join("token"))
}

pub fn load_token() -> Result<Option<String>> {
    load_token_from(&token_path()?)
}

fn save_token(token: &str) -> Result<()> {
    save_token_at(token, &token_path()?)
}

fn save_token_at(token: &str, path: &std::path::Path) -> Result<()> {
    use std::io::Write as _;
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .context("Failed to create token file")?;
        f.write_all(token.as_bytes())
            .context("Failed to write token")?;
    }
    #[cfg(not(unix))]
    {
        fs::write(path, token).context("Failed to save token")?;
    }
    Ok(())
}

fn load_token_from(path: &std::path::Path) -> Result<Option<String>> {
    if path.exists() {
        let token = fs::read_to_string(path)
            .context("Failed to read token file")?
            .trim()
            .to_string();
        if token.is_empty() {
            return Ok(None);
        }
        Ok(Some(token))
    } else {
        Ok(None)
    }
}

pub fn logout() -> Result<()> {
    let path = token_path()?;
    if path.exists() {
        fs::remove_file(&path)?;
    }
    Ok(())
}

pub async fn login(
    api_url: &str,
    method: Option<LoginMethod>,
    login: Option<&str>,
    password_env: &str,
) -> Result<()> {
    let method = match method {
        Some(method) => method,
        None => prompt_login_method()?,
    };

    match method {
        LoginMethod::Telegram => login_telegram(api_url).await,
        LoginMethod::Account => login_account(api_url, login, password_env).await,
    }
}

/// Run the Telegram login flow: start local server, open browser, capture code, exchange for JWT.
pub async fn login_telegram(api_url: &str) -> Result<()> {
    // Bind to a random port on 127.0.0.1
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");

    let auth_url = format!(
        "{}/auth/telegram/start?redirect_uri={}",
        api_url.trim_end_matches('/'),
        urlencoding(&redirect_uri)
    );

    eprintln!("Opening browser for Telegram login...");
    eprintln!("If it doesn't open, visit: {auth_url}");

    if open::that(&auth_url).is_err() {
        eprintln!("Failed to open browser automatically.");
    }

    // Wait for the callback
    let code = wait_for_callback(listener).await?;

    // Exchange code for JWT
    let auth = ApiClient::exchange_code(api_url, &code).await?;
    save_token(&auth.token)?;

    let name = auth
        .user
        .username
        .as_deref()
        .or(auth.user.first_name.as_deref())
        .unwrap_or("user");

    eprintln!("Logged in as {name} (id: {})", auth.user.id);

    Ok(())
}

/// Log in with a Floppa account login + password.
///
/// The password is read from the configured environment variable when present, otherwise from a
/// hidden terminal prompt. Do not pass passwords as command-line arguments.
pub async fn login_account(api_url: &str, login: Option<&str>, password_env: &str) -> Result<()> {
    let login = match login {
        Some(login) => login.trim().to_string(),
        None => prompt_line("Login: ")?,
    };
    if login.is_empty() {
        bail!("Login cannot be empty");
    }

    let password = match std::env::var(password_env) {
        Ok(password) => password,
        Err(std::env::VarError::NotPresent) => rpassword::prompt_password("Password: ")?,
        Err(err) => return Err(err).with_context(|| format!("Failed to read {password_env}")),
    };
    if password.is_empty() {
        bail!("Password cannot be empty");
    }

    let auth = ApiClient::login_account(api_url, &login, &password).await?;
    save_token(&auth.token)?;

    let name = auth
        .user
        .username
        .as_deref()
        .or(auth.user.first_name.as_deref())
        .unwrap_or("user");

    eprintln!("Logged in as {name} (id: {})", auth.user.id);

    Ok(())
}

fn prompt_login_method() -> Result<LoginMethod> {
    if !io::stdin().is_terminal() {
        bail!(
            "Interactive method selection requires a TTY. Pass `--method telegram` or `--method account`."
        );
    }

    eprintln!("Choose login method:");
    eprintln!("1) Telegram");
    eprintln!("2) Account login + password");

    loop {
        let choice = prompt_line("Method [1/2]: ")?;
        match choice.trim() {
            "1" | "t" | "T" | "telegram" | "Telegram" => return Ok(LoginMethod::Telegram),
            "2" | "a" | "A" | "account" | "Account" => return Ok(LoginMethod::Account),
            "" => eprintln!("Please enter 1 or 2."),
            _ => eprintln!("Unknown method `{choice}`. Enter 1 for Telegram or 2 for account."),
        }
    }
}

fn prompt_line(prompt: &str) -> Result<String> {
    let mut stdout = io::stdout();
    write!(stdout, "{prompt}")?;
    stdout.flush()?;

    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

/// Wait for a single HTTP GET request on the callback listener, extract `code` param.
async fn wait_for_callback(listener: TcpListener) -> Result<String> {
    let (mut stream, _) =
        tokio::time::timeout(std::time::Duration::from_secs(120), listener.accept())
            .await
            .context("Timeout waiting for browser callback")?
            .context("Failed to accept callback connection")?;

    // Read until we see the end of HTTP headers (\r\n\r\n).
    let mut buf = Vec::with_capacity(4096);
    let deadline = std::time::Duration::from_secs(10);
    tokio::time::timeout(deadline, async {
        let mut tmp = [0u8; 512];
        loop {
            let n = stream.read(&mut tmp).await?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        Ok::<_, std::io::Error>(())
    })
    .await
    .context("Timeout reading browser callback")?
    .context("Failed to read callback request")?;

    let request = String::from_utf8_lossy(&buf);

    // Parse GET /callback?code=XYZ HTTP/1.1
    let code = request
        .lines()
        .next()
        .and_then(|line| {
            let path = line.split_whitespace().nth(1)?;
            let query = path.split('?').nth(1)?;
            query.split('&').find_map(|param| {
                let (k, v) = param.split_once('=')?;
                if k == "code" {
                    Some(v.to_string())
                } else {
                    None
                }
            })
        })
        .ok_or_else(|| anyhow!("No 'code' parameter in callback"))?;

    // Respond with success page
    let body = r#"<!DOCTYPE html>
<html><head><title>Floppa VPN</title></head>
<body style="font-family:system-ui;display:flex;justify-content:center;align-items:center;height:100vh;margin:0">
<div style="text-align:center">
<h1>Login successful!</h1>
<p>You can close this tab and return to the terminal.</p>
</div></body></html>"#;

    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );

    stream.write_all(response.as_bytes()).await?;
    stream.flush().await?;

    Ok(code)
}

fn urlencoding(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::task::JoinHandle;

    async fn spawn_account_login_server() -> (String, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let n = stream.read(&mut buf).await.unwrap();
            let request = String::from_utf8_lossy(&buf[..n]);
            let (head, body) = request.split_once("\r\n\r\n").unwrap();

            assert!(head.starts_with("POST /auth/account/login HTTP/1.1"));
            assert!(
                head.contains("content-type: application/json")
                    || head.contains("Content-Type: application/json")
            );
            assert!(!head.contains("authorization:"));

            let body: serde_json::Value = serde_json::from_str(body).unwrap();
            assert_eq!(body["login"], "alice");
            assert_eq!(body["password"], "s3cret");

            let response_body = serde_json::json!({
                "token": "server-token",
                "user": {
                    "id": 42,
                    "username": "alice",
                    "first_name": null,
                    "is_admin": false
                }
            });
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_body.to_string().len(),
                response_body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            stream.flush().await.unwrap();
        });
        (base_url, handle)
    }

    fn temp_token_path(test_name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("floppa-cli-{test_name}-{unique}.token"))
    }

    #[tokio::test]
    async fn login_account_posts_credentials_and_token_can_be_saved() {
        let token_path = temp_token_path("login-account");

        let (base_url, server) = spawn_account_login_server().await;
        let auth = crate::api::ApiClient::login_account(&base_url, "alice", "s3cret")
            .await
            .unwrap();
        server.await.unwrap();

        assert_eq!(auth.token, "server-token");

        save_token_at(&auth.token, &token_path).unwrap();
        assert_eq!(
            load_token_from(&token_path).unwrap().as_deref(),
            Some("server-token")
        );
        fs::remove_file(token_path).unwrap();
    }

    #[derive(clap::Parser)]
    struct LoginMethodArgs {
        #[arg(long, value_enum)]
        method: Option<LoginMethod>,
    }

    #[test]
    fn login_method_value_enum_accepts_account_and_telegram() {
        let account = LoginMethodArgs::parse_from(["test", "--method", "account"]);
        assert_eq!(account.method, Some(LoginMethod::Account));

        let telegram = LoginMethodArgs::parse_from(["test", "--method", "telegram"]);
        assert_eq!(telegram.method, Some(LoginMethod::Telegram));
    }
}
