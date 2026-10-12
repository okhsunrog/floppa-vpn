//! Fetch transport for the generated frontend SDK. No endpoint-specific API duplication.
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use tauri::State;

#[derive(Default)]
pub struct NetworkState {
    origin: OnceLock<String>,
    pending: Mutex<HashMap<String, PendingRequest>>,
}

struct PendingRequest {
    notify: Arc<tokio::sync::Notify>,
    cancelled: bool,
    created: Instant,
}
impl Default for PendingRequest {
    fn default() -> Self {
        Self {
            notify: Arc::new(tokio::sync::Notify::new()),
            cancelled: false,
            created: Instant::now(),
        }
    }
}

#[tauri::command]
#[specta::specta]
pub fn configure_http(base: String, state: State<'_, NetworkState>) -> Result<(), String> {
    let url = checked_url(&base)?;
    let origin = url.origin().ascii_serialization();
    if state.origin.get_or_init(|| origin.clone()) != &origin {
        return Err("HTTP origin is already configured".into());
    }
    Ok(())
}
fn checked_url(raw: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(raw).map_err(|_| "invalid request URL")?;
    let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if url.scheme() != "https" && !(cfg!(debug_assertions) && url.scheme() == "http" && local) {
        return Err("HTTP requests require HTTPS".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("URL credentials are not supported".into());
    }
    Ok(url)
}

#[derive(Deserialize, specta::Type)]
pub struct HttpRequest {
    id: String,
    url: String,
    method: String,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
}
#[derive(Serialize, specta::Type)]
pub struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

#[tauri::command]
#[specta::specta]
pub fn cancel_http(id: String, state: State<'_, NetworkState>) {
    if id.len() > 128 {
        return;
    }
    let mut pending = state.pending.lock().unwrap();
    pending.retain(|_, entry| entry.created.elapsed() < Duration::from_secs(30));
    if pending.len() >= 128 && !pending.contains_key(&id) {
        return;
    }
    let slot = pending.entry(id).or_default();
    slot.cancelled = true;
    slot.notify.notify_one();
}

#[tauri::command]
#[specta::specta]
pub async fn native_http(
    request: HttpRequest,
    state: State<'_, NetworkState>,
) -> Result<HttpResponse, String> {
    if request.id.len() > 128 {
        return Err("invalid request ID".into());
    }
    let url = checked_url(&request.url)?;
    if state.origin.get() != Some(&url.origin().ascii_serialization()) {
        return Err("request origin is not allowed".into());
    }
    if request
        .body
        .as_ref()
        .is_some_and(|b| b.len() > 4 * 1024 * 1024)
    {
        return Err("request body too large".into());
    }
    let notify = {
        let mut pending = state.pending.lock().unwrap();
        pending.retain(|_, entry| entry.created.elapsed() < Duration::from_secs(30));
        if pending.len() >= 128 {
            return Err("too many HTTP requests".into());
        }
        let slot = pending.entry(request.id.clone()).or_default();
        if slot.cancelled {
            pending.remove(&request.id);
            return Err("request aborted".into());
        }
        slot.notify.clone()
    };
    let result = tokio::select! {
        biased;
        _ = notify.notified() => Err("request aborted".into()),
        result = tokio::time::timeout(Duration::from_secs(20), send(&request, url)) => result.unwrap_or_else(|_| Err("request timed out".into())),
    };
    state.pending.lock().unwrap().remove(&request.id);
    result
}

async fn send(request: &HttpRequest, url: reqwest::Url) -> Result<HttpResponse, String> {
    let method = reqwest::Method::from_bytes(request.method.as_bytes())
        .map_err(|_| "invalid HTTP method")?;
    if matches!(method, reqwest::Method::CONNECT | reqwest::Method::TRACE) {
        return Err("unsupported HTTP method".into());
    }
    let mut builder = floppa_net::client_builder()
        .build()
        .map_err(|e| e.to_string())?
        .request(method, url);
    for (name, value) in &request.headers {
        // Transport framing and routing must not be controlled by frontend headers.
        if matches!(
            name.to_ascii_lowercase().as_str(),
            "host" | "connection" | "content-length" | "transfer-encoding" | "proxy-authorization"
        ) {
            continue;
        }
        builder = builder.header(name, value);
    }
    if let Some(body) = &request.body {
        builder = builder.body(body.clone());
    }
    let req = builder.build().map_err(|e| e.without_url().to_string())?;
    let mut response = floppa_net::execute(req).await?;
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.to_string(), v.to_string())))
        .collect();
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| e.without_url().to_string())?
    {
        if body.len() + chunk.len() > 8 * 1024 * 1024 {
            return Err("response body too large".into());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(HttpResponse {
        status,
        headers,
        body,
    })
}
