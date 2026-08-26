# Client Logging Architecture

## Overview

All logging flows through Rust's `tracing` crate. On Android, output goes to logcat
via `tracing-logcat` (tag: `FloppaVPN`). On desktop, output goes to stdout with ANSI
colors. Frontend JS logs are bridged into the same system.

## Log Sources and Targets

There are three log sources, each producing tracing events with a specific target:

| Source | Target | Example |
|--------|--------|---------|
| Our Rust code (`tracing::info!()` etc.) | the module path it is written in — `floppa_vpn_core::…` for the tunnel, `floppa_client_lib::…` for the app shell | `INFO floppa_vpn_core::store: stored config` |
| Frontend JS (`console.*`) | `webview` | `INFO webview: [web] Frontend initialized` |
| Rust `log` crate (keyring, etc.) | `log` | `DEBUG log: creating entry with service floppa-vpn` |

### Why the frontend has a target of its own

The frontend used to reach the log through `@tauri-apps/plugin-log`, which hands the line to
the Rust `log` crate; `tracing-log` bridges that, and every bridged record arrives as the *same*
tracing callsite — target `log`, with the real source demoted to a `log.target` field. Third-party
crates arrive that way too, so the two could not be filtered apart: `log=warn`, which is there to
keep a chatty dependency quiet, also silenced the frontend, and `console.info` — the level the
frontend is told to write at — never reached logcat at all.

The frontend now calls a command of ours, `webview_log(level, message)`, which emits under target
`webview` directly. A tracing target is fixed at its callsite, so the command is a match over the
level with one `tracing::event!` per arm — five callsites, and `webview=…` becomes a real filter
directive. `tauri-plugin-log` is gone: with `skip_logger()` it provided nothing else.

### Desktop: `ShortTargetFormat`

On desktop, `ShortTargetFormat` shortens targets for cleaner output:
- `log` → the real source from the `log.target` field (e.g. `keyring`)
- `webview:error@http://localhost:1420/node_modules/...` → `webview`, for the lines Tauri's own
  WebView interception produces
- Other targets (e.g. `floppa_client_lib::vpn::config`, and now `webview` itself) are left as-is.

This formatter is **not used on Android** (`#[cfg(not(target_os = "android"))]`)
because Android already shows short targets.

## Frontend Console Forwarding

`setupConsoleForwarding()` in `main.ts` patches `console.log/debug/info/warn/error` to also call
`commands.webviewLog(level, message)`. This ensures all frontend `console.*` calls (including from
shared code in `floppa-web-shared`) appear in tracing output.

The original `console.*` function is still called, so browser DevTools work normally. The invoke's
rejection is swallowed: reporting it would reach `console.error`, which is this very function.

Mapping: `console.log` → `trace`, `console.debug` → `debug`, `console.info` → `info`,
`console.warn` → `warn`, `console.error` → `error`.

**Write at `console.info` and above.** `console.log` maps to trace and is filtered out in the
normal profile — deliberately, so the noisy default level stays out of a user's logs.

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
| `floppa_vpn_core` (the tunnel: actor, backends, platform, rollback, store) | `info` | `trace` |
| `floppa_client_lib` (the app's own shell) | `info` | `trace` |
| `floppa_cli` | `info` | `trace` |
| `gotatun` (WireGuard/AmneziaWG tunnel) | `info` | `trace` |
| `shoes_lite` (VLESS tunnel) | `info` | `trace` |
| `webview` (frontend `console.*`) | `info` | `trace` |
| `log` (`log`-crate bridge: keyring and other deps) | `warn` | `debug` |
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
- `setupConsoleForwarding()` → `webview_log` command → target `webview`
- Tauri may also intercept WebView console → target `webview:LEVEL@URL`, shortened for display

### Android

- Output: logcat via `tracing-logcat` (tag: `FloppaVPN`)
- `setupConsoleForwarding()` → `webview_log` command → target `webview`
- No `ShortTargetFormat` — targets are already short (filter by tag `FloppaVPN`)

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

## The `log` crate bridge

We never register a `log` logger of our own. `tracing-subscriber`'s `tracing-log` feature is on
by default, so `try_init()` installs `LogTracer`, and records from dependencies that log through
the `log` crate arrive as tracing events with target `log` and a `log.target` field naming the
real source. That is what the `log=…` directive addresses — dependencies only, now that the
frontend has a target of its own.
