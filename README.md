# Floppa VPN CLI

<p align="center">
  <img src="branding/logo-transparent.png" alt="Floppa VPN logo" width="180" />
</p>

CLI-only fork/branch for the headless `floppa-cli` utility. This branch intentionally does not include the Tauri desktop client, web admin UI, server, daemon, migrations, mobile platform glue, or packaging assets from the upstream monorepo.

## What is included

- `floppa-cli` Rust binary.
- Root Cargo workspace and lockfile for reproducible CLI builds.
- Linux CLI networking code for WireGuard/AmneziaWG and VLESS+REALITY tunnels.
- Telegram login/auth flow used by the CLI.
- Minimal README with build/run/test commands.

## CLI work in this branch

This branch focuses on improving the original CLI utility instead of adding a wrapper around it.

Implemented CLI-side improvements:

- Stable local `device_id` stored under the user config directory.
- Peer reuse by `device_id + protocol` via the API.
- Peer lifecycle commands:
  - `floppa-cli peer delete --peer-id <id>`
  - `floppa-cli peer delete --protocol amneziawg`
  - `floppa-cli peer delete --all`
  - `floppa-cli vless regenerate`
  - `floppa-cli device show`
  - `floppa-cli device reset`
- Built-in local status command without contacting the API:
  - `floppa-cli status`
  - `floppa-cli status --interface floppa0`
- Idempotent Linux route handling with `ip route replace`.
- Cleanup guard for DNS and routes on Ctrl+C/SIGTERM/error paths.
- Basic route/interface verification after tunnel setup.

## Build and test

```bash
cargo fmt --check
cargo check -p floppa-cli
cargo test -p floppa-cli
```

## Run

```bash
cargo run -p floppa-cli -- --help
cargo run -p floppa-cli -- login
cargo run -p floppa-cli -- connect --protocol amneziawg
cargo run -p floppa-cli -- status
```

Connecting to a real VPN requires the user's Floppa account/session and Linux network privileges. Do not commit private configs, tokens, or VPN keys.

## Why this branch exists

The upstream repository contains the full cross-platform product: desktop client, web UI, server, daemon, mobile platform code, packaging, and integration tests. For CLI hardening and headless use cases, this branch keeps only the code needed to build, test, and run `floppa-cli`.
