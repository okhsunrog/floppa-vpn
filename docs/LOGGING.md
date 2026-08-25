# Client Logging Architecture

## Overview

All logging flows through Rust's `tracing` crate. On Android, output goes to logcat
via `tracing-logcat` (tag: `FloppaVPN`). On desktop, output goes to stdout with ANSI
colors. Frontend JS logs are bridged into the same system.

## Log Sources and Targets

There are three log sources, each producing tracing events with a specific target:

| Source | Target | Example |
|--------|--------|---------|
| Our Rust code (`tracing::info!()` etc.) | `floppa_client_lib::module::path` | `INFO floppa_client_lib::vpn::config: WG config loaded` |
| Frontend JS (`console.*` via plugin-log) | `log` | `INFO log: [web] Frontend initialized` |
| Rust `log` crate (keyring, etc.) | `log` | `DEBUG log: creating entry with service floppa-vpn` |

### Why target is `log`, not `webview`

`@tauri-apps/plugin-log` JS functions (`info()`, `error()`, etc.) route through the
Rust `log` crate, which `tracing-subscriber` bridges via `tracing-log`. This bridge
assigns target `log` to all events.

There is a `tracing` feature flag on `tauri-plugin-log` that makes the plugin emit
directly to `tracing` with target `webview` + a `location` field. However, this creates
**duplicate events** because Tauri's built-in WebView console interception also fires
for `console.*` calls, producing a second event with target `webview:LEVEL@URL`.
We don't use the `tracing` feature to avoid these duplicates.

### Desktop: `ShortTargetFormat`

On desktop, `ShortTargetFormat` renames targets for cleaner output:
- `log` → `webview` (displayed name only)
- `webview:error@http://localhost:1420/node_modules/...` → `webview`
- Other targets (e.g. `floppa_client_lib::vpn::config`) are left as-is.

This formatter is **not used on Android** (`#[cfg(not(target_os = "android"))]`)
because Android already shows short targets.

## Frontend Console Forwarding

`setupConsoleForwarding()` in `main.ts` patches `console.log/debug/info/warn/error`
to also call the corresponding `@tauri-apps/plugin-log` function. This ensures all
frontend `console.*` calls (including from shared code in `floppa-web-shared`) appear
in tracing output.

The original `console.*` function is still called, so browser DevTools work normally.

Mapping: `console.log` → `trace()`, `console.debug` → `debug()`, `console.info` → `info()`,
`console.warn` → `warn()`, `console.error` → `error()`.

## Filter Levels

The filter is built at runtime from `LogConfig` (`logging.rs`), not from the build profile. A
`LogConfig` is persisted as `log-config.json` in the log directory (`logging::get_log_dir()`),
loaded by `init_tracing` and swapped at runtime through a `tracing_subscriber::reload` handle
(`apply_log_config`; in the `:vpn` process the UI pushes it over RPC). It holds:

- `profile: LogProfile` — `Normal` (default) or `Verbose`
- `custom_filter: Option<String>` + `custom_filter_enabled: bool` — a raw `RUST_LOG`-style
  directive string that, when enabled and valid, replaces the profile entirely

Both profiles start from `EnvFilter::from_default_env()` with a `warn` base level and add:

| Target | `Normal` | `Verbose` |
|--------|----------|-----------|
| `floppa_client_lib` (our Rust code) | `info` | `trace` |
| `gotatun` (WireGuard/AmneziaWG tunnel) | `info` | `trace` |
| `shoes_lite` (VLESS tunnel) | `info` | `trace` |
| `webview` (Tauri console interception) | `warn` | `debug` |
| `log` (frontend + `log`-crate bridge) | `warn` | `debug` |
| `tarpc` (Android IPC) | `warn` | `trace` |
| everything else | `warn` | `warn` |

## Processes and Diagnostic Captures

`init_tracing(log_dir, LogProcess)` is called once per process. `LogProcess` is `Ui` (the Tauri
process) or `Vpn` (the Android `:vpn` service process); both build the same subscriber, and the
enum decides only two things: the name of the capture file (`ui.log` / `vpn.log`) and what to
do with a leftover `active-capture` marker.

A diagnostic capture copies log events into `<log_dir>/captures/<capture_id>/<process>.log`
while it is active; logcat/stdout output continues regardless. In the UI process the capture is
owned by `logging::capture::CaptureSession`, held in Tauri state: start, stop, status and export
run under one lock for their whole duration, so two starts cannot race and a stop cannot
interleave with the start it undoes. The UI owns the `active-capture` marker file: it writes it
when a capture starts and removes it when the capture stops. The `:vpn` process reads the marker at startup, so a service restart mid-capture
resumes writing to the same capture. A marker found by a starting UI means the previous UI died
mid-capture, so the UI removes it instead of resuming.

Each process's capture file is capped at 64 MiB (`MAX_CAPTURE_BYTES`). Writes past the cap are
dropped rather than rotated — a capture is meant to be minutes long, and its beginning is the
useful part — so a forgotten capture cannot fill the disk.

## Platform Differences

### Desktop (Linux/Windows)

- Output: stdout with ANSI colors via `ShortTargetFormat`
- `setupConsoleForwarding()` → plugin-log → `log` crate → `tracing-log` → target `log`
- `ShortTargetFormat` renames `log` → `webview` for display
- Tauri may also intercept WebView console → target `webview:LEVEL@URL` (deduplicated)

### Android

- Output: logcat via `tracing-logcat` (tag: `FloppaVPN`)
- `setupConsoleForwarding()` → Tauri WebView console interception → target `webview:LEVEL@URL`
- Direct `info()`/`error()` calls → target `webview:<anonymous>@http://tauri.localhost/...`
- No `ShortTargetFormat` — long targets visible in logcat but harmless (filter by tag `FloppaVPN`)

## Reading Logs

### Desktop
Logs appear in the terminal running `vp exec tauri dev`.

### Android (adb)
```bash
# Quick: justfile commands
just app-logs                    # Show recent FloppaVPN logs
just deploy-android-test         # Build, install, restart, show logs

# Manual
adb logcat -d --pid=$(adb shell pidof dev.okhsunrog.floppa_vpn) -s FloppaVPN
```

## Plugin Configuration

In `lib.rs`:
```rust
.plugin(tauri_plugin_log::Builder::new().skip_logger().build())
```

`skip_logger()` prevents the plugin from registering its own global `log` logger,
since we have our own `tracing-subscriber` setup in `logging.rs`.

## Cargo Features

```toml
tauri-plugin-log = { version = "2", features = ["colored"] }
```

The `colored` feature adds ANSI colors to the plugin's internal formatting.
The `tracing` feature is **not used** — see "Why target is `log`" above.
