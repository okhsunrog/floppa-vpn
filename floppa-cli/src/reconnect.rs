//! Auto-reconnect for the VPN tunnel.
//!
//! Two independent triggers wake the reconnect loop:
//!
//! 1. **DBus resume** — on Linux with systemd-logind, subscribe to
//!    `org.freedesktop.login1.Manager`'s `PrepareForSleep` signal. On resume
//!    (`PrepareForSleep(false)`) we wake the loop immediately instead of
//!    waiting for the next watchdog tick. This is what makes "close the lid,
//!    open it, VPN is already back" feel instant.
//!
//! 2. **Watchdog** — every `watchdog_interval` the loop probes the tunnel's
//!    health. For WireGuard/AmneziaWG it reads the live peer handshake via the
//!    kernel UAPI and checks the newest handshake is still fresh; for
//!    VLESS+REALITY it does a TCP connect probe to the endpoint.
//!
//! On a failed health check (or an external wake) the tunnel is torn down and
//! rebuilt. Rebuild is idempotent so waking repeatedly never leaks interfaces
//! or routes. Retryable errors back off exponentially; fatal errors surface
//! so the process exits and (under systemd `Restart=on-failure`) is restarted.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::watch;

/// Boxed future that does not require `Send` — the reconnect loop drives these
/// futures on its own task and never ships them to another thread.
pub type BoxFutureLocal<T> = Pin<Box<dyn Future<Output = T> + 'static>>;

/// Tunable reconnect behaviour.
#[derive(Clone, Copy, Debug)]
pub struct ReconnectConfig {
    /// How often the watchdog probes tunnel health.
    pub watchdog_interval: Duration,
    /// A WireGuard handshake older than this (or missing entirely) means down.
    pub handshake_stale_after: Duration,
    /// Initial reconnect delay, doubled each attempt.
    pub backoff_base: Duration,
    /// Upper bound for the exponential backoff.
    pub backoff_max: Duration,
    /// Hard cap on reconnect attempts before giving up (0 = unlimited).
    pub max_attempts: u32,
}

impl Default for ReconnectConfig {
    fn default() -> Self {
        Self {
            watchdog_interval: Duration::from_secs(30),
            handshake_stale_after: Duration::from_secs(2 * 60 + 30),
            backoff_base: Duration::from_secs(2),
            backoff_max: Duration::from_secs(60),
            max_attempts: 0,
        }
    }
}

/// A signal source that wakes the reconnect loop. The `Receiver` fires on
/// `changed()` whenever a wake is requested (sleep resume, or a manual nudge).
#[derive(Clone)]
pub struct ReconnectSignal {
    tx: watch::Sender<()>,
    rx: watch::Receiver<()>,
}

impl Default for ReconnectSignal {
    fn default() -> Self {
        let (tx, rx) = watch::channel(());
        Self { tx, rx }
    }
}

impl ReconnectSignal {
    /// Request an immediate reconnect check. Safe to call from any thread.
    pub fn wake(&self) {
        // Ignore send errors: no receiver means the loop already ended.
        let _ = self.tx.send(());
    }

    fn subscribe(&self) -> watch::Receiver<()> {
        self.rx.clone()
    }
}

/// Health check for an established tunnel.
///
/// Returns `Ok(true)` when the tunnel is healthy, `Ok(false)` when it should
/// be torn down and rebuilt, and `Err` only for unexpected internal errors
/// (not for a mere "not connected" condition).
pub type HealthCheck = Box<dyn FnMut() -> BoxFutureLocal<Result<bool>>>;

/// (Re)build the tunnel. Called once on start and again after every detected
/// drop. Should return `Ok(())` when the tunnel is up, or `Err` describing
/// what failed (used to decide backoff vs. fatal abort).
pub type Rebuild = Box<dyn FnMut() -> BoxFutureLocal<Result<()>>>;

/// Run the reconnect loop until the tunnel is intentionally stopped (the
/// `shutdown` future resolves) or a fatal, non-retryable error occurs.
///
/// `health` is polled every `watchdog_interval`. `rebuild` brings the tunnel
/// up; it is always called at least once (the initial connect) before the
/// first health check. `signal` lets external triggers (DBus resume) request
/// an out-of-band check.
pub async fn run(
    config: ReconnectConfig,
    mut health: HealthCheck,
    mut rebuild: Rebuild,
    signal: &ReconnectSignal,
    mut shutdown: impl Future<Output = ()> + Unpin,
) -> Result<()> {
    let mut wake_rx = signal.subscribe();

    let mut attempts: u32 = 0;
    // Initial connect.
    rebuild_tunnel(&mut rebuild, &mut attempts, &config, "connect").await?;

    loop {
        let sleep = tokio::time::sleep(config.watchdog_interval);
        tokio::select! {
            _ = &mut shutdown => {
                tracing::info!("reconnect: shutdown requested, leaving loop");
                return Ok(());
            }
            // External wake (e.g. DBus resume): skip the remaining wait.
            res = wake_rx.changed() => {
                if res.is_ok() {
                    tracing::info!("reconnect: external wake received, checking tunnel");
                }
            }
            _ = sleep => {}
        }

        match health().await {
            Ok(true) => {
                attempts = 0;
            }
            Ok(false) => {
                tracing::warn!("reconnect: tunnel unhealthy, rebuilding");
                rebuild_tunnel(&mut rebuild, &mut attempts, &config, "reconnect").await?;
            }
            Err(e) => {
                // Health probe itself failed — treat as a drop and rebuild.
                tracing::warn!("reconnect: health check error ({e:#}), rebuilding");
                rebuild_tunnel(&mut rebuild, &mut attempts, &config, "reconnect").await?;
            }
        }
    }
}

/// Attempt to (re)build the tunnel with exponential backoff.
///
/// `label` is the log prefix (`"connect"` on first call, `"reconnect"` after).
/// Returns the first `Ok` from `rebuild`, or the last error if `max_attempts`
/// is exhausted or a fatal error is encountered (surfaced to the caller).
async fn rebuild_tunnel(
    rebuild: &mut Rebuild,
    attempts: &mut u32,
    config: &ReconnectConfig,
    label: &str,
) -> Result<()> {
    loop {
        match rebuild().await {
            Ok(()) => {
                *attempts = 0;
                return Ok(());
            }
            Err(e) => {
                // Non-retryable: surface immediately so systemd can restart us.
                if !is_retryable(&e) {
                    return Err(e).with_context(|| format!("reconnect: fatal {label} failure"));
                }

                if config.max_attempts != 0 && *attempts >= config.max_attempts {
                    return Err(e).with_context(|| {
                        format!(
                            "reconnect: {label} failed after {} attempts",
                            config.max_attempts
                        )
                    });
                }

                let delay = backoff(*attempts, config);
                *attempts += 1;
                tracing::warn!(
                    "reconnect: {label} failed (attempt {}) — {e:#}; retrying in {:.1}s",
                    *attempts,
                    delay.as_secs_f64()
                );
                tokio::time::sleep(delay).await;
            }
        }
    }
}

/// Exponential backoff: base * 2^attempt, capped at `backoff_max`.
fn backoff(attempt: u32, config: &ReconnectConfig) -> Duration {
    let mult: u64 = 1u64.checked_shl(attempt).unwrap_or(u64::MAX);
    let base_secs = config.backoff_base.as_secs_f64();
    let scaled = base_secs * mult as f64;
    Duration::from_secs_f64(scaled.min(config.backoff_max.as_secs_f64()))
}

/// Decide whether an error is worth retrying. Network/IO errors are retryable;
/// config/parse errors are fatal (would loop forever on the same bad input).
fn is_retryable(e: &anyhow::Error) -> bool {
    e.chain().any(|cause| {
        if cause
            .downcast_ref::<reqwest::Error>()
            .is_some_and(|re| re.is_connect() || re.is_timeout())
        {
            return true;
        }
        if cause.downcast_ref::<std::io::Error>().is_some() {
            return true;
        }
        false
    })
}

/// Spawn a task that watches systemd-logind `PrepareForSleep` signals and
/// wakes the reconnect loop on resume.
///
/// Returns the joined task handle. If DBus is unavailable (non-systemd host,
/// macOS, or no session bus), the task logs once and exits — the watchdog
/// alone still covers reconnection; it just won't react instantly to sleep.
#[cfg(target_os = "linux")]
pub fn spawn_resume_watcher(signal: ReconnectSignal) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(e) = watch_logind(signal).await {
            tracing::warn!("reconnect: DBus sleep-resume watcher disabled: {e:#}");
        }
    })
}

#[cfg(not(target_os = "linux"))]
pub fn spawn_resume_watcher(_signal: ReconnectSignal) -> tokio::task::JoinHandle<()> {
    // No systemd-logind off Linux; the watchdog covers everything.
    tokio::spawn(async {})
}

#[cfg(target_os = "linux")]
async fn watch_logind(signal: ReconnectSignal) -> Result<()> {
    use ordered_stream::OrderedStreamExt;
    use zbus::{Connection, Proxy};

    let conn = Connection::system()
        .await
        .context("failed to open system D-Bus connection")?;

    let proxy = Proxy::new(
        &conn,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )
    .await
    .context("failed to create logind proxy")?;

    let mut stream = proxy
        .receive_signal("PrepareForSleep")
        .await
        .context("failed to subscribe to PrepareForSleep signal")?;

    tracing::info!("reconnect: watching systemd-logind for sleep/resume");
    while let Some(msg) = stream.next().await {
        let start: bool = match msg.body().deserialize() {
            Ok(b) => b,
            Err(_) => continue,
        };
        if !start {
            tracing::info!("reconnect: system resumed from sleep, waking loop");
            signal.wake();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_then_caps() {
        let cfg = ReconnectConfig {
            backoff_base: Duration::from_secs(2),
            backoff_max: Duration::from_secs(60),
            ..Default::default()
        };
        assert_eq!(backoff(0, &cfg), Duration::from_secs(2));
        assert_eq!(backoff(1, &cfg), Duration::from_secs(4));
        assert_eq!(backoff(2, &cfg), Duration::from_secs(8));
        assert_eq!(backoff(5, &cfg), Duration::from_secs(60)); // capped
        assert_eq!(backoff(10, &cfg), Duration::from_secs(60)); // still capped
    }

    #[test]
    fn signal_wake_is_cloneable_and_sendable() {
        let sig = ReconnectSignal::default();
        let sig2 = sig.clone();
        sig.wake();
        sig2.wake();
    }

    #[test]
    fn default_config_sane() {
        let cfg = ReconnectConfig::default();
        assert!(cfg.watchdog_interval.as_secs() >= 5);
        assert!(cfg.backoff_base <= cfg.backoff_max);
    }
}
