# CLAUDE.md

## Project Overview

Floppa VPN — multi-protocol VPN service (AmneziaWG default, WireGuard, VLESS+REALITY): Telegram bot + admin web panel + desktop/mobile client. Moscow VPS (WireGuard/AmneziaWG server) → Europe VPS (exit node, NAT). Deployed via Ansible (`cloud-forge` repo). Android package: `dev.okhsunrog.floppavpn`.

## Commands

```bash
just check          # fmt + clippy + tests + frontend type-check + lint — run before committing
just client-check   # Client half only (tauri crates + floppa-web-shared/floppa-client + kotlin)
just server-check   # Server half only (workspace crates + floppa-face) — needs a live DB for sqlx
just fmt            # Format all (Rust + frontend)
just openapi        # Regenerate OpenAPI TS client (no running backend needed)
just build-android  # Release APK (aarch64)
just package        # build-frontend → cargo build → deployment archive
just test-integration  # E2E VPN tests (Docker + tests/integration/.env + .secrets/*.conf)

cd floppa-face && vp dev         # Admin panel dev (proxies /api → :3000)
cd floppa-client && vp dev       # Client dev + regenerates specta bindings
```

Frontend uses [Vite+](https://viteplus.dev/) (`vp`) as the unified toolchain — see the [Frontend](#frontend) section.

## Server Architecture

```
                PostgreSQL (source of truth)
                       │
    ┌──────────────────┼──────────────────┐
floppa-server                       floppa-daemon
(teloxide bot + Axum API +          (WireGuard sync,
 embedded Vue via memory-serve)      tc rate limits, root)
    └──────── pg LISTEN/NOTIFY ───────────┘
```

Coordination: server writes peer `sync_status = 'pending_add'` → DB trigger fires `pg_notify('peer_changed')` → daemon syncs WireGuard → sets `'active'`. All stateless, DB is source of truth.

**Rust workspace** (all crates at root level, edition 2024):
- `floppa-core` — models, DB, config, crypto (ChaCha20-Poly1305), WG key gen (x25519-dalek), business logic (`services.rs`: upsert_user with auto-trial, peer creation, IP allocation)
- `floppa-server` — Axum + teloxide + Vue embedded via `memory-serve`. OpenAPI via `utoipa`
- `floppa-daemon` — `wg`/`awg set` sync (WireGuard + AmneziaWG, one interface per protocol, peer `protocol` column routes it), `wg show dump` bandwidth, Linux tc HFSC per-peer rate limits
- `floppa-vless` — VLESS+REALITY proxy, shared DB with server
- `floppa-cli` — standalone CLI client, `--protocol wireguard|amneziawg|vless` (also used as tunnel binary for integration tests)
- `floppa-client/src-tauri` — Tauri desktop/mobile app (Rust backend)
- `tauri-plugin-vpn` — Android VPN plugin (Kotlin + Rust)

## Client VPN Architecture

**Desktop (Linux/Windows):** Single process. `VpnBackend` trait → gotatun (Mullvad's Rust WireGuard). `Platform` trait handles routes/DNS/TUN. Graceful cleanup on exit via `RunEvent::Exit` in `lib.rs`.

**Android:** Two-process model for VPN to survive app swipe-close:
- UI process: Tauri WebView + Rust commands
- `:vpn` process: `FloppaVpnService` (Kotlin) → JNI → gotatun Rust tunnel
- IPC: tarpc over Unix socket (`vpn.sock` in app data dir)

**`tauri-plugin-vpn/`** — Kotlin plugin for Android VPN lifecycle, TUN creation, split tunneling, foreground notification, device info (`Build.MODEL`), safe area insets. Rust side exposes `VpnExt` trait → `run_mobile_plugin` → Kotlin `@Command` methods.

**Key files** in `floppa-client/src-tauri/src/vpn/`:
- `commands.rs` — Tauri commands with `#[cfg]` branches for Android vs desktop
- `backend.rs` — `VpnBackend` trait (start/stop/stats/handshake)
- `platform.rs` — `Platform` trait (routes, DNS, TUN, cleanup)
- `config.rs` — device identity, WG config persistence (OS keyring on desktop, file on Android)

## Database

PostgreSQL + sqlx (compile-time checked). Migrations in `migrations/` (daemon auto-runs on startup).

Tables: `users` (`is_admin`, `trial_used_at`), `peers` (`sync_status` enum, traffic counters), `subscriptions` (speed/traffic limits), `plans` (seeded by migration). Auto-trial: 7-day Basic on first user creation (`floppa_core::services::upsert_user`).

## Configuration

Two TOML files (see `*.example.toml`):
- **config.toml** (`FLOPPA_CONFIG`, default `/etc/floppa-vpn/config.toml`): WG interface/endpoint/subnet/DNS, rate limits, bot username, JWT expiration
- **secrets.toml** (`FLOPPA_SECRETS`, default `/etc/floppa-vpn/secrets.toml`): database_url, wg_private_key, bot token, jwt_secret, encryption_key, admin_telegram_ids

## Frontend

**Bun workspace — 3 packages:**
- `floppa-face` — admin panel (Vue 3 + Nuxt UI v4 + Vite+). Embedded into server binary via `memory-serve`. Dev: proxies `/api` → `:3000`
- `floppa-client` — Tauri 2 client (Linux, Windows, Android). Overrides dashboard (adds VpnCard via `#vpn-widget` slot) and login (deep-link auth) routes
- `floppa-web-shared` — ALL views, components, router (`createAppRoutes`, `installAuthGuard`), Pinia auth store, OpenAPI client, Pinia Colada queries, i18n (en/ru), format utils

**Toolchain — [Vite+](https://viteplus.dev/) (`vp`):** unified frontend toolchain (dev/build/lint/format), wraps bun. Detected via `bun.lock` + root `packageManager` field. ESLint **and** Prettier were removed in favor of `vp`'s built-in oxlint + oxfmt — config lives in the **root `vite.config.ts`** (`lint` + `fmt` blocks, globs resolved from root); per-package `vite.config.ts` files hold only Vite/framework config and import `defineConfig` from `'vite-plus'`. The `vite`/`vite-plus`/`vitest` versions are pinned via the bun `catalog:` in the root `package.json`.

```bash
vp dev                    # dev server (run inside floppa-face / floppa-client)
vp build                  # production build (Rolldown-based)
vp lint                   # oxlint, type-aware — run at root to cover the whole workspace
vp fmt src/               # oxfmt format (vp fmt --check to verify)
vp check                  # format + lint + type checks together
```

The `just client-check` / `just server-check` / `just fmt` recipes and CI delegate to these via the package.json `lint`/`format`/`build` scripts; `vue-tsc --build` is still used for full type-checking. The `*.vue` import shim is intentionally absent — vue-tsc resolves SFCs natively, and a generic shim would mask prop-type errors.

**UI:** Nuxt UI v4 via `@nuxt/ui/vite` + `@nuxt/ui/vue-plugin` (no Nuxt framework). Components auto-imported. `useToast()` auto-imported.

**Auto-generated — NEVER edit manually:**
- `floppa-web-shared/src/client/` — OpenAPI TS client. Regenerate: `just openapi`
- `floppa-client/src/bindings.ts` — tauri-specta bindings. Regenerate: `cd floppa-client && bun tauri dev` (exports at app startup, not compile time). Commands registered in `lib.rs` via `tauri_specta::Builder`

**Data fetching:** Pinia Colada — `useQuery(getStatsQuery())`, `useMutation(createPlanMutation())` from `@pinia/colada.gen`. Auth interceptors in each app's `main.ts`.

**i18n:** All locales in `floppa-web-shared/src/locales/` (en/ru). No per-app locale files.

**Tailwind v4 gotcha:** Must add `@source` in each app's CSS to scan shared components:
- `floppa-face/src/assets/main.css`: `@source "../../../floppa-web-shared/src";`
- `floppa-client/src/styles.css`: `@source "../../floppa-web-shared/src";`

Auth: Telegram Login Widget → JWT in localStorage → Bearer header.
