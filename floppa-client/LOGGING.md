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

### Debug builds (`cfg(debug_assertions)`)

| Directive | Effect |
|-----------|--------|
| `floppa_client_lib=trace` | All our Rust logs |
| `webview=trace` | WebView console interception (if any) |
| `tauri=info` | Tauri framework logs |
| default: `debug` | Everything else at DEBUG+ |

### Release builds

| Directive | Effect |
|-----------|--------|
| `floppa_client_lib=debug` | Our Rust logs at DEBUG+ |
| `webview=info` | WebView interception at INFO+ |
| `log=info` | Frontend + log-crate at INFO+ |
| `gotatun=info` | WireGuard tunnel at INFO+ |
| `tarpc=warn` | Android IPC at WARN+ |
| default: `warn` | Everything else at WARN+ |

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
Logs appear in the terminal running `bun tauri dev`.

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
