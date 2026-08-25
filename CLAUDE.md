# CLAUDE.md

## Project Overview

Floppa VPN — multi-protocol VPN service (AmneziaWG default, WireGuard, VLESS+REALITY): Telegram bot + admin web panel + desktop/mobile client. Moscow VPS (WireGuard/AmneziaWG server, VLESS proxy behind HAProxy) → Europe VPS (exit node, NAT). Deployed via Ansible (`cloud-forge` repo, see `docs/DEPLOYMENT.md`). Android application id: `dev.okhsunrog.floppa_vpn` (Kotlin package `dev.okhsunrog.floppavpn`).

## Commands

```bash
just check          # fmt + clippy + tests + frontend type-check + lint — run before committing
just client-check   # Client half only (tauri crates + floppa-web-shared/floppa-client + kotlin; Android clippy if cargo-ndk is installed)
just server-check   # Server half only (workspace crates + floppa-face + `just machete`) — needs a live DB for sqlx
just lint           # clippy (workspace + client crates, --all-targets) + vp check, no auto-fix
just machete        # cargo-machete over every crate (workspace, floppa-client/src-tauri, tauri-plugin-vpn)
just fmt            # Format all (Rust + frontend + Kotlin)
just openapi        # Regenerate OpenAPI TS client (no running backend needed)
just bindings       # Regenerate tauri-specta bindings (no running app needed)
just build-android  # Release APK (arm64 only, split per ABI) — release.yml runs the same recipe
just package        # build-frontend → cargo build → deployment archive
just test-integration  # E2E VPN tests (Docker + tests/integration/.env + .secrets/*.conf)

cd floppa-face && vp dev         # Admin panel dev (proxies /api → :3000)
cd floppa-client && vp dev       # Client dev + regenerates specta bindings
```

Frontend uses [Vite+](https://viteplus.dev/) (`vp`) as the unified toolchain — see the [Frontend](#frontend) section. `rust-toolchain.toml` pins the channel and the `aarch64-linux-android` target; CI and release workflows rely on it rather than adding targets by hand.

## Server Architecture

```
                PostgreSQL (source of truth)
                       │
    ┌──────────────────┼──────────────────┐
floppa-server                       floppa-daemon        floppa-vless
(teloxide bot + Axum API +          (wg/awg sync,        (VLESS+REALITY proxy,
 embedded Vue via memory-serve)      tc rate limits, root) user registry from DB)
    └──────── pg LISTEN/NOTIFY ───────────┴──────────────────┘
```

Coordination: server writes peer `sync_status = 'pending_add'` → DB trigger fires `pg_notify('peer_changed')` → daemon syncs the peer to its protocol's interface and applies its tc limit → sets `'active'` (a peer stays `pending_add` until both succeeded). VLESS has no peers: `users.vless_uuid` + `vless_user_changed`/`subscription_changed` notifications feed the proxy's registry. All stateless, DB is source of truth.

**Rust workspace** (all crates at root level, edition 2024):
- `floppa-core` — models (typed `Protocol` with `FromStr`/`Display`/`TryFrom<&str>`, `PeerSyncStatus`, `SubscriptionSource` — sqlx TEXT + serde), DB, config (`Config.client_subnet` is `Ipv4Network`, `get_server_ip() -> Ipv4Addr`, `BotConfig.web_app_url: Option<url::Url>`), crypto (ChaCha20-Poly1305), WG key gen (x25519-dalek), typed `FloppaError`. Business logic in `services.rs`: `upsert_user` (auto-trial), `create_peer(ctx, user_id, CreatePeerOptions)`, IP allocation, `replace_active_subscription` (the one subscription writer), `ensure_vless_uuid`, `mark_peer_for_removal`, `calculate_proration(current, price, period_days, now)`; `password::{hash_password, verify_password, dummy_verify}` are async
- `floppa-server` — Axum + teloxide + Vue embedded via `memory-serve`. OpenAPI via `utoipa`
- `floppa-daemon` — `SyncContext` (one per interface, `target()` picks `WgTool::{Wg, Awg}`), `wg show dump` → `PeerStat` for bandwidth, `check_expired` (handles `active` and `pending_add`), Linux tc HFSC in `tc.rs`: root `1:`, total class `1:1`, default class `1:ffff` for unlimited peers, per-peer class minor = host offset within the /16 **rendered as hex**, u32 filter at `prio = <offset in decimal>`, ingress via `ifb-<iface minus wg->`; `tc::set_peer_limit` adds or updates
- `floppa-vless` — VLESS+REALITY proxy (shoes-lite), shared DB with server, own config/secrets (`FLOPPA_VLESS_CONFIG`/`FLOPPA_VLESS_SECRETS`)
- `floppa-cli` — standalone CLI client: `connect --protocol wireguard|amneziawg|vless`, token via `--token-file`/`FLOPPA_TOKEN_FILE` (default `<config dir>/floppa-cli/token`, sudo-aware) or `--token`/`FLOPPA_TOKEN`, persistent `device_id` file, DNS via `resolvectl` under systemd-resolved, clean exit on SIGINT/SIGTERM/SIGHUP (also the tunnel binary for integration tests)
- `floppa-client/src-tauri` — Tauri desktop/mobile app (Rust backend); excluded from the workspace
- `tauri-plugin-vpn` — Android VPN plugin (Kotlin + Rust); excluded from the workspace

## Client VPN Architecture

**Desktop (Linux/Windows):** Single process. `VpnBackend` trait → gotatun (Mullvad's Rust WireGuard, AmneziaWG fork) or shoes-lite (VLESS). `Platform` trait handles routes/DNS/TUN. Graceful cleanup on exit via `RunEvent::Exit` in `lib.rs`.

**Android:** Two-process model for VPN to survive app swipe-close:
- UI process: Tauri WebView + Rust commands
- `:vpn` process: `FloppaVpnService` (Kotlin) → JNI (`nativeInit` / `nativeStartServer` / `nativeStop` in `vpn/jni_entry.rs`) → Rust tunnel
- IPC: tarpc over Unix socket (`vpn.sock` in app data dir); `create_backend(socket_path, app_handle)` on Android

**`tauri-plugin-vpn/`** — **Android only.** `src/android.rs` holds the implementation (`Vpn<R>`, async `VpnExt::vpn()` → `run_mobile_plugin_async` → Kotlin `@Command` methods); on other platforms `init()` registers nothing. `Error { Register, PluginInvoke }`. Kotlin side: VPN lifecycle, TUN creation, split tunneling, foreground notification, device info (`Build.MODEL`), safe area insets (`docs/SAFE-AREA-AND-Z-INDEX.md`). No iOS implementation (`docs/IOS-BACKEND-PLAN.md`).

**Key files** in `floppa-client/src-tauri/src/`:
- `vpn/commands.rs` — Tauri commands with `#[cfg]` branches for Android vs desktop
- `vpn/backend/` — `VpnBackend` trait (`mod.rs`), `in_process.rs` (desktop), `android_ipc.rs` (tarpc client)
- `vpn/platform/` — `Platform` trait (`mod.rs`: routes, DNS, TUN, `default_gateway(family) -> Gateway(IpAddr)`), `linux.rs`, `windows.rs`, `android.rs`
- `vpn/actor/` — connection state machine (intents, epochs, reconcile); `vpn/rollback/` — journal of undo steps (`Step::EndpointRoute` is durable across restarts)
- `vpn/config.rs` — device identity; VPN config persistence as an envelope `{updated_at, configs}`: desktop writes the OS keyring first with a 0600 file fallback (newest copy wins, file migrates back into the keyring); Android is file only (0600, private app dir)
- `logging.rs` — `init_tracing(log_dir, LogProcess::{Ui, Vpn})`, `LogProfile::{Normal, Verbose}` + custom filter, diagnostic captures capped at 64 MiB (`docs/LOGGING.md`)

## Database

PostgreSQL + sqlx (compile-time checked). Migrations in `migrations/`, sequential `NNNN_description.sql` (daemon auto-runs on startup). After changing any `query!`/`query_as!`/`query_scalar!` run `just sqlx-prepare` (`cargo sqlx prepare --workspace -- --all-targets` — test targets included, since CI's clippy job and `just lint` compile them under `SQLX_OFFLINE`) and commit `.sqlx/`; `just sqlx-check` is the CI check.

Tables: `users` (`is_admin`, `trial_used_at`, `vless_uuid`), `peers` (`sync_status`, `protocol` — both CHECK-constrained since 0014), `subscriptions` (`source` CHECK-constrained, speed/traffic limits; `payment_id` dropped in 0015), `plans` (seeded by migration, `trial_minutes`). A user has at most one current subscription: `subscriptions.is_current` (partial unique index, 0017) is written only by `services::replace_active_subscription` (+ the merge path), and every reader gets "the user's subscription" from the `current_subscriptions` view (`is_active` = current and not expired) — never from `subscriptions` with an `ORDER BY … LIMIT 1`. Traffic counters live in VictoriaMetrics, not in the DB. Auto-trial on first user creation (`floppa_core::services::upsert_user`).

## Configuration

Two TOML files (see `*.example.toml`):
- **config.toml** (`FLOPPA_CONFIG`, default `/etc/floppa-vpn/config.toml`): WG/AWG interface/endpoint/subnet/DNS, rate limits, AWG obfuscation params, bot username, JWT expiration, `[vless]`, `[metrics]`; top-level keys (`min_client_version`, `allowed_origins`) must stay above the first table
- **secrets.toml** (`FLOPPA_SECRETS`, default `/etc/floppa-vpn/secrets.toml`): database_url, wg_private_key, awg_private_key, bot token, jwt_secret, encryption_key, admin_telegram_ids, `[vless]` REALITY keys. `0600` when hand-installed; Ansible renders `root:floppa 0640` (see `docs/DEPLOYMENT.md`)

## Frontend

**Bun workspace — 3 packages:**
- `floppa-face` — admin panel (Vue 3 + Nuxt UI v4 + Vite+). Embedded into server binary via `memory-serve`. Dev: proxies `/api` → `:3000`
- `floppa-client` — Tauri 2 client (Linux, Windows, Android). Overrides dashboard (adds VpnCard via `#vpn-widget` slot) and login (deep-link auth) routes. `src/config.ts` requires `VITE_API_URL` at startup; `vpnStore.error` is a typed `VpnError` union; `settingsStore.manualProtocol`
- `floppa-web-shared` — ALL views, components, router (`createAppRoutes`, `installAuthGuard` — redirects on logout), Pinia auth store, OpenAPI client, Pinia Colada queries, i18n (`createSharedI18n` in `i18n.ts`, en/ru), `api/interceptors.ts` (`installApiInterceptors`: auth header, token refresh, 426), `utils/apiError.ts` (`isApiError`, `describeError`, `ApiErrorCode`), `utils/telegram.ts`, `utils/platform.ts`, format utils, composables `useInvalidateQueries` / `useSearchFilter` / `useLocaleSwitch`

**Toolchain — [Vite+](https://viteplus.dev/) (`vp`):** unified frontend toolchain (dev/build/lint/format), wraps bun. Detected via `bun.lock` + root `packageManager` field. ESLint **and** Prettier were removed in favor of `vp`'s built-in oxlint + oxfmt — config lives in the **root `vite.config.ts`** (`lint` + `fmt` blocks, globs resolved from root); per-package `vite.config.ts` files hold only Vite/framework config and import `defineConfig` from `'vite-plus'`. The `vite`/`vite-plus`/`typescript` versions are pinned via the bun `catalog:` in the root `package.json`, and the bun version via `packageManager` — CI reads both, so those two fields are the single place a toolchain version is chosen.

```bash
vp dev                    # dev server (run inside floppa-face / floppa-client)
vp build                  # production build (Rolldown-based)
vp check                  # format + lint + type checks in one pass over the workspace
vp check --fix            # ...and apply what can be fixed
vp test --run             # all workspace tests (they live beside the code they cover)
```

`build`, `test` and `typecheck` are Vite+ built-in tasks, not package.json scripts — the packages
define only what Vite+ has no built-in for (`dev`, `preview`, `openapi-ts`, `sync-version`). The
`just` recipes and CI both go through `vp check`; `vue-tsc --build` still runs separately as the
`typecheck` task, because `vp check`'s type pass does not resolve `.vue` SFCs. For that pass each package's `env.d.ts` declares a generic `*.vue` module shim (`DefineComponent<Record<string, unknown>, …>`); it satisfies `vp check` only, and it is `vue-tsc` — resolving SFCs natively — that catches prop-type errors, so both must stay green.

**UI:** Nuxt UI v4 via `@nuxt/ui/vite` + `@nuxt/ui/vue-plugin` (no Nuxt framework). Components auto-imported. `useToast()` auto-imported.

**Auto-generated — NEVER edit manually:**
- `floppa-web-shared/src/client/` — OpenAPI TS client. Regenerate: `just openapi`
- `floppa-client/src/bindings.ts` — tauri-specta bindings. Regenerate: `just bindings` (~1s; runs the `export_bindings` binary, no app start needed). `vp exec tauri dev` also refreshes them on startup. Commands registered in `lib.rs` via `tauri_specta::Builder`

**Data fetching:** Pinia Colada — `useQuery(getStatsQuery())`, `useMutation(createPlanMutation())` from `@pinia/colada.gen`. Each app's `main.ts` calls `installApiInterceptors(client, authStore, …)` from the shared package.

**i18n:** All locales in `floppa-web-shared/src/locales/` (en/ru). No per-app locale files. `locales.test.ts` enforces en/ru key parity.

**Tailwind v4 gotcha:** Must add `@source` in each app's CSS to scan shared components:
- `floppa-face/src/assets/main.css`: `@source "../../../floppa-web-shared/src";`
- `floppa-client/src/styles.css`: `@source "../../floppa-web-shared/src";`

Auth: Telegram Login Widget → JWT in localStorage → Bearer header. Every JWT carries a `jti` naming a `sessions` row (revocable per device or "everywhere" via `users.tokens_valid_after`); tokens issued before sessions existed have no `jti` and are accepted until they expire.
