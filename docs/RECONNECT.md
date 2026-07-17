# Auto-reconnect

floppa-cli keeps the tunnel up automatically. Two independent mechanisms
detect a dropped connection and bring it back without user interaction.

## Why a tunnel drops

- **System sleep / suspend** — the network interface goes down, the WireGuard
  handshake stops, and after ~3 minutes the peer is considered dead by the
  server. On resume the OS has a new IP / new routes and the old tunnel is
  silently broken.
- **Wi-Fi / Ethernet roaming** — switching APs or plugging a cable changes the
  default route; in-flight packets die.
- **Transient server / ISP hiccups** — brief outages, CG-NAT rebinding.

## Detection

### 1. Instant wake on resume (Linux + systemd)
floppa-cli subscribes to systemd-logind's `PrepareForSleep` D-Bus signal.
When the system resumes (`PrepareForSleep(false)`), the reconnect loop is
woken immediately — no need to wait for the next watchdog tick. This is what
makes "close the lid, open it, VPN is already back" feel instant.

If D-Bus / logind is unavailable (macOS, containers without a session bus,
non-systemd hosts) this watcher logs once and disables itself; the watchdog
below still covers everything — it just reacts on the next interval rather
than instantly.

### 2. Watchdog (all platforms)
Every `watchdog_interval` (default 30 s) the loop runs a health probe:

- **WireGuard / AmneziaWG** — reads the live peer stats via the wireguard
  UAPI (`device.read`) and checks the *newest* handshake across all peers.
  If it is older than `handshake_stale_after` (default 2 min 30 s) — or
  missing entirely — the tunnel is considered down.
- **VLESS+REALITY** — opens a TCP connect to the configured endpoint with a
  3 s timeout. Reachable ⇒ healthy, otherwise ⇒ down.

## Rebuild

On a failed health check (or an external wake) the tunnel is torn down and
rebuilt:

1. Previous routes / DNS / interface are cleaned up (`CleanupKind`).
2. `create_tunnel` brings the interface back up (idempotent — it removes any
   stale interface first).
3. Networking (policy routing, DNS) is re-applied.
4. The first handshake (WG) or TCP reachability (VLESS) is verified.

Because rebuild is idempotent you can wake it repeatedly without leaking
interfaces or routes.

## Backoff

Repeated failures use exponential backoff: `backoff_base × 2^attempt`,
capped at `backoff_max` (defaults: 2 s base, 60 s cap). Network / IO errors
are retried; config / parse errors are treated as fatal and surfaced so the
process exits and (under systemd) is restarted by `Restart=on-failure`.

`max_attempts` (default 0 = unlimited) bounds retries if you want the client
to give up instead of retrying forever.

## Tuning

The behaviour is controlled by `reconnect::ReconnectConfig`:

| Field                  | Default        | Meaning                                   |
|------------------------|----------------|-------------------------------------------|
| `watchdog_interval`    | 30 s           | Health-check period                       |
| `handshake_stale_after`| 2 min 30 s     | WG handshake age that counts as "dead"    |
| `backoff_base`         | 2 s            | Initial reconnect delay                   |
| `backoff_max`          | 60 s           | Backoff ceiling                           |
| `max_attempts`         | 0 (unlimited)  | Retry cap before giving up                |

## Logs

Watch stderr for:

```
Connected! Auto-reconnect is on. Press Ctrl+C or send SIGTERM to disconnect.
```
```
reconnect: system resumed from sleep, waking loop
reconnect: reconnect failed (attempt 1) — ...; retrying in 2.0s
reconnect: tunnel rebuilt successfully
```

Set `RUST_LOG=debug` (or `info`) for the full trail — see [LOGGING.md](LOGGING.md).
