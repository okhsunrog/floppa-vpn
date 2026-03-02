# Floppa VPN

WireGuard VPN service built with Rust — daemon, Telegram bot, admin panel, and Tauri 2 client app.

[![CI](https://github.com/okhsunrog/floppa-vpn/actions/workflows/ci.yml/badge.svg)](https://github.com/okhsunrog/floppa-vpn/actions/workflows/ci.yml)

## Architecture

```mermaid
graph TD
    Client["<b>Client Apps</b><br/>Tauri 2 — Linux, Windows, Android"]

    Client -- "WireGuard :51820" --> Daemon
    Client -- "HTTPS" --> Nginx

    subgraph VPS
        Nginx["<b>Nginx</b><br/>Reverse proxy + TLS"]
        Nginx -- ":3000" --> Server

        Server["<b>floppa-server</b><br/>Telegram bot · REST API · Vue admin panel"]
        Server <-- "pg LISTEN / NOTIFY" --> DB

        DB[("<b>PostgreSQL</b><br/>Source of truth")]
        DB <-- "pg LISTEN / NOTIFY" --> Daemon

        Daemon["<b>floppa-daemon</b><br/>WireGuard sync · tc HFSC rate limits · bandwidth tracking"]
    end
```

**How it works:** Server writes peer changes to PostgreSQL (e.g. `sync_status = 'pending_add'`) → DB trigger fires `pg_notify('peer_changed')` → daemon picks it up, syncs WireGuard, applies rate limits, and marks peer as `active`. All state lives in the database.

## Features

### Daemon
- Stateless WireGuard peer synchronization via `wg set`
- Per-peer HFSC traffic shaping (bidirectional — egress + IFB ingress)
- Bandwidth tracking from `wg show dump`
- Auto-runs database migrations on startup

### Telegram Bot
- User registration with automatic 7-day trial
- Subscription status, language switching (en/ru)
- Inline button to open the web app

### Admin Panel
- Dashboard with server stats and traffic overview
- User management — create, search, subscription control
- Plan management — speed limits, traffic caps, peer limits, pricing
- Peer monitoring — sync status, traffic, last handshake

### Client App (Tauri 2)
- Cross-platform: Linux, Windows, Android
- Split tunneling with per-app selection (Android)
- WireGuard config persistence via OS keyring (desktop) or encrypted file (Android)
- Deep-link authentication (Telegram Login Widget → JWT)
- Two-process architecture on Android (VPN survives app swipe-close)

## Client Architecture

The client uses trait-based abstraction (`VpnBackend` + `Platform`) to share Tauri commands across platforms while handling OS differences underneath.

### Desktop (Linux, Windows)

```mermaid
graph LR
    subgraph "Single Process"
        WebView["Vue WebView"]
        Commands["Tauri Commands"]
        Backend["InProcessBackend"]
        Tunnel["gotatun tunnel<br/>(Mullvad WireGuard)"]
        Platform["Platform trait<br/>Linux: pkexec + helper script<br/>Windows: netsh"]
    end

    WebView -- "tauri-specta" --> Commands
    Commands --> Backend
    Backend --> Tunnel
    Commands --> Platform
    Platform -- "routes, DNS,<br/>TUN device" --> OS["OS Network Stack"]
```

Single-process: gotatun runs the WireGuard tunnel in-process. The `Platform` trait handles OS-specific network setup — Linux uses a polkit helper script for privilege escalation, Windows uses `netsh`. Config is persisted in the OS keyring (secret-service / DPAPI). Graceful cleanup on exit restores DNS and routes.

### Android

```mermaid
graph LR
    subgraph "UI Process"
        WebView["Vue WebView"]
        Commands["Tauri Commands"]
        Plugin["tauri-plugin-vpn<br/>(Kotlin ↔ Rust)"]
        IPC_Client["tarpc client"]
    end

    subgraph ":vpn Process"
        Service["FloppaVpnService<br/>(foreground service)"]
        JNI["JNI bridge"]
        Tunnel["gotatun tunnel"]
        IPC_Server["tarpc server"]
    end

    WebView -- "tauri-specta" --> Commands
    Commands --> Plugin
    Plugin -- "startService()" --> Service
    Service -- "TUN fd" --> JNI
    JNI --> Tunnel
    IPC_Client <-- "Unix socket<br/>(stats, stop)" --> IPC_Server
    Tunnel -- "protectSocket()<br/>via JNI" --> Service
```

Two-process model so the VPN survives app swipe-close. The custom `tauri-plugin-vpn` bridges Kotlin and Rust — it launches `FloppaVpnService` as a foreground service in a separate `:vpn` process, which creates the TUN device and passes the fd to gotatun via JNI. The UI process communicates with the VPN process over a tarpc Unix socket for stats and stop commands. `protectSocket()` calls back into Kotlin via JNI to prevent WireGuard's UDP packets from routing through the VPN itself. Split tunneling uses Android's per-app VPN API (`addAllowedApplication` / `addDisallowedApplication`).

## Tech Stack

| Layer | Tech |
|-------|------|
| Server | Rust, Axum, teloxide, sqlx, utoipa (OpenAPI), memory-serve |
| Daemon | Rust, WireGuard (`wg`), Linux tc, sqlx |
| Frontend | Vue 3, Nuxt UI v4, Pinia Colada, Tailwind v4 |
| Client | Tauri 2, gotatun (Mullvad WireGuard), tauri-specta (type-safe bindings), custom tauri-plugin-vpn |
| Database | PostgreSQL with LISTEN/NOTIFY |
| Crypto | x25519-dalek (WG keys), ChaCha20-Poly1305 (storage), JWT |

## Development

```bash
# Prerequisites: Rust toolchain, bun, just

# Install frontend dependencies
bun install

# Run all checks (fmt, clippy, tests, type-check, lint)
just check

# Dev servers
cd floppa-face && bun dev        # Admin panel (proxies /api → :3000)
cd floppa-client && bun tauri dev      # Client app

# Regenerate OpenAPI TypeScript client
just openapi

# Build Android APK
just build-android

# Build deployment archive (frontend + server binaries)
just package
```

## Deployment

See [DEPLOYMENT.md](DEPLOYMENT.md) for the full guide. TL;DR: Ansible deploys three systemd services — `floppa-daemon` (root, WireGuard + tc), `floppa-server` (bot + API + embedded frontend), and nginx as reverse proxy with Let's Encrypt.

## License

[GPL-3.0](LICENSE)
