use anyhow::{Context, Result, anyhow};
use std::fs;
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::api::ApiClient;

fn config_dir() -> Result<PathBuf> {
    let dir = dirs::config_dir()
        .ok_or_else(|| anyhow!("Cannot determine config directory"))?
        .join("floppa-cli");
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn token_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("token"))
}

pub fn load_token() -> Result<Option<String>> {
    let path = token_path()?;
    if path.exists() {
        let token = fs::read_to_string(&path)
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

fn save_token(token: &str) -> Result<()> {
    let path = token_path()?;
    fs::write(&path, token).context("Failed to save token")?;
    // Restrict permissions
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

pub fn logout() -> Result<()> {
    let path = token_path()?;
    if path.exists() {
        fs::remove_file(&path)?;
    }
    Ok(())
}

/// Run the login flow: start local server, open browser, capture code, exchange for JWT.
pub async fn login(api_url: &str) -> Result<()> {
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

/// Wait for a single HTTP GET request on the callback listener, extract `code` param.
async fn wait_for_callback(listener: TcpListener) -> Result<String> {
    let (mut stream, _) = listener
        .accept()
        .await
        .context("Failed to accept callback connection")?;

    let mut buf = vec![0u8; 4096];
    let n = stream.read(&mut buf).await?;
    let request = String::from_utf8_lossy(&buf[..n]);

    // Parse GET /callback?code=XYZ HTTP/1.1
    let code = request
        .lines()
        .next()
        .and_then(|line| {
            let path = line.split_whitespace().nth(1)?;
            let query = path.split('?').nth(1)?;
            query.split('&').find_map(|param| {
                let (k, v) = param.split_once('=')?;
                if k == "code" { Some(v.to_string()) } else { None }
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
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            _ => format!("%{:02X}", c as u8),
        })
        .collect()
}
