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
just vpn-status     # Tunnel on the device: off/busy/connected (`vpn-connect` / `vpn-disconnect` drive it)
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
- `floppa-core` — models (typed `Protocol` with `FromStr`/`Display`/`TryFrom<&str>`, `PeerSyncStatus`, `SubscriptionSource` — sqlx TEXT + serde), DB, config (`Config::parse` is the one entry point — strict `deny_unknown_fields` on every section; `[wireguard]` and `[amneziawg]` are both a `TunnelInterfaceConfig` whose AmneziaWG-only `obfuscation` is rejected under the former and defaulted under the latter, and whose optional `mtu` (rendered into client configs, applied to the interface's link by the daemon, bounded to 576..=1500 and to >=1280 when `allowed_ips` carries IPv6) is defaulted only under the latter; `client_subnet` is `Ipv4Network`, `get_server_ip() -> Ipv4Addr`, `BotConfig.web_app_url: Option<url::Url>`), crypto (ChaCha20-Poly1305), WG key gen (x25519-dalek), typed `FloppaError`. Business logic in `services.rs`: `upsert_user` (auto-trial), `create_peer(ctx, user_id, CreatePeerOptions)`, IP allocation, `replace_active_subscription` (the one subscription writer), `ensure_vless_uuid`, `mark_peer_for_removal`, `calculate_proration(current, price, period_days, now)`; `password::{hash_password, verify_password, dummy_verify}` are async
- `floppa-server` — Axum + teloxide + Vue embedded via `memory-serve`. OpenAPI via `utoipa`
- `floppa-daemon` — `wg::ensure_interface(tool, &TunnelInterfaceConfig, private_key)` converges each interface at startup, `SyncContext` (one per interface, `target()` picks `WgTool::{Wg, Awg}`), `wg show dump` → `PeerStat` for bandwidth, `check_expired` (handles `active` and `pending_add`), Linux tc HFSC in `tc.rs`: root `1:`, total class `1:1`, default class `1:ffff` for unlimited peers, per-peer class minor = host offset within the /16 **rendered as hex**, u32 filter at `prio = <offset in decimal>`, ingress via `ifb-<iface minus wg->`; `tc::set_peer_limit` adds or updates
- `floppa-vless` — VLESS+REALITY proxy (shoes-lite), shared DB with server, own config/secrets (`FLOPPA_VLESS_CONFIG`/`FLOPPA_VLESS_SECRETS`)
- `floppa` — the command-line client, and also **the system service**: `floppa service` (Linux, root) holds the actor, the store and the rollback journal in a process that outlives every client, so the tunnel survives closing the app and comes back after a reboot. One binary, not two — `src/service.rs`. It serves on the socket systemd passed it (`inherited_listener`) or binds `/run/floppa-vpn/vpn.sock` itself when started by hand, and in that second case makes the directory and the socket root-only, because who else may drive the tunnel is the `.socket` unit's answer to give and there is no unit to read it from. State lives in `/var/lib/floppa-vpn` with `config::use_file_storage_only()`: a root process has no D-Bus session, so every keyring call is a blocking round trip that fails — on load and on *every save*. It takes a `SessionSink`, because a client's credentials have to be kept somewhere root-owned and readable at boot, and the payload crosses the wire **opaque** so `floppa-vpn-core` never learns what a server session is
  **`connect` has two shapes, chosen by what the caller supplied, not by a flag.** With `--config` it builds an actor in this process and holds the tunnel until interrupted (root; `ConfigSource::Ephemeral`) — that is what `tests/integration/conftest.py` runs, in a container with no service, so it must keep working. Without `--config` it asks the service, seeds it with the session, and returns: the tunnel outlives the command. `disconnect` and `status` are the service's too. Which mode a client is in comes from `client_mode::probe`, which **connects** rather than checking the file (a dead process leaves its socket behind) and keeps `Forbidden` separate from `Absent` — being refused by the socket means the user is not in the `floppa` group, and silently falling back to the in-process path would work slightly worse forever for a reason nobody would be shown
- `floppa` (the same crate) is also a standalone client, and a thin one: `connect` imports the config into the same actor the app runs (`Deployment { iface, manage_dns, ConfigSource::Ephemeral }`) and waits, so it has the protocol ladder, the reconnect budget, silence detection and the rollback journal rather than its own straight-line version of them. What is left here is clap, the login flow (loopback listener + `--token-file`/`FLOPPA_TOKEN_FILE`, default `<config dir>/floppa/token`, sudo-aware — a `floppa-cli` directory from before the rename is moved on first use), and asking the server for one protocol's config; the device identity is `floppa-provision`'s, not its own. Also the tunnel binary for the integration tests, which is why the actor is exercised on a desktop by every run
- `floppa-api-client` — the client side of the server API, shared by `floppa` and `floppa-client`: `schema.rs` is **generated** from `floppa-web-shared/openapi.json` by `just openapi` (never edited), `client.rs` is the one `reqwest` client over it with failures typed by the server's error code, `provision.rs` is the peer logic. Its load-bearing rule: `peer_by_device` returns `Result<Option<_>, _>`, so "the server says there is no peer" and "the server could not be asked" cannot be confused
- `floppa-provision` — keeping this device's peers on the server, from whichever process holds the actor. It sits **above** `floppa-vpn-core` and `floppa-api-client`, which is why it is its own crate: the core runs a tunnel and knows nothing about who provisioned it, the API client describes the server and knows nothing about tunnels, and neither should gain the other as a dependency. `identity.rs` (the one device id and device name — the server tells installations apart by that id, so a second generator of it is a second device eating the account's peer limit; the directory is a parameter because it genuinely differs per caller, and a pre-unification bare `device_id` file is adopted rather than replaced), `session.rs` (who this device is to the server, in a file the actor's process can read — the token lives in a webview's `localStorage` and the process that needs it has no webview), `server.rs` (`ActorSink`: a fetched config lands in the actor's store, over the socket where there is one), `outcome.rs` (`plan_outcome`: a finished cycle read as "a peer may have been deleted"), `watcher.rs` (acting on it with nobody looking — started in the process the actor is in, and nowhere else, or two watchers replace the same peer).
  **Nothing in this crate reads `env!("CARGO_PKG_VERSION")`**: it is linked into the app, the command-line client and (soon) the service, so the version it would report is its own, not the running binary's. `app_version` is a parameter of `identity::device_identity`, `ServerSession::identity`, `server::client` and `watcher::watch`, and every caller passes its own `env!("CARGO_PKG_VERSION")`
- `floppa-vpn-core` — everything about the client-side tunnel, shared by `floppa` and `floppa-client/src-tauri`: the connection actor, the tunnel backends, the platform layer, the rollback journal, the config store and the logging setup. No sqlx and no tauri. On Linux the privileged helper goes through `pkexec` as a user and runs directly as root (`sudo floppa`, and the test containers)
- `xtask` — repository chores that need Rust: `cargo run -p xtask -- api-types` writes `floppa-api-client/src/schema.rs`, and `just openapi` runs it
- `floppa-tunnel-config` — client-side tunnel config shared by `floppa` and `floppa-client`: the strict WireGuard/AmneziaWG `.conf` parser (`TunnelConfig`, typed `ConfigParseError`), `AwgObfuscation` (re-exported by `floppa-core::config`), VLESS tunnel defaults, pure route helpers (`route::{endpoint_route, split_default, parse_default_route, pick_endpoint}`) and, behind the `gotatun` feature, the gotatun peer/device builders. No sqlx/tauri/tokio; anything that resolves names, creates TUNs or runs `ip`/`netsh` stays in the clients
- `floppa-client/src-tauri` — the Tauri app's own shell: the command surface, the event bridge, the process starter and the JNI entry. Everything about the tunnel is `floppa-vpn-core`. Excluded from the workspace, depends on the shared crates by path
- `tauri-plugin-vpn` — Android VPN plugin (Kotlin + Rust); excluded from the workspace

## Client VPN Architecture

**Desktop (Linux/Windows):** Single process. `VpnBackend` trait → gotatun (Mullvad's Rust WireGuard, AmneziaWG fork) or shoes-lite (VLESS). `Platform` trait handles routes/DNS/TUN. Graceful cleanup on exit via `RunEvent::Exit` in `lib.rs`.

**Android:** two processes, and **all the decisions are in `:vpn`** — the intent, the status, the
connect ladder, the reconnect budget and the config store all live there, beside the tunnel. The UI
process holds a socket to them and no tunnel state at all. The move happened because Android's
cached-app freezer stops the UI process outright: an actor living there could not reconnect a tunnel
that died while the phone was in a pocket, and a swipe-close left the tunnel running unwatched. See
`docs/ANDROID-TUNNEL-PROCESS.md` for the full design and the device matrix.

- `:vpn` process: `FloppaVpnService` (Kotlin) hosts the actor. `nativeInit(logDir, dataDir)` boots
  it on the first service instance (logging, config dir, `TunnelManager`, `ServiceRegistry`, the
  actor, the socket) and only refreshes the JNI callback reference on later ones. What crosses the
  JNI boundary is what only Kotlin can do: `hasConsent()`, `startGeneration(planJson, generation)`
  → `nativeSetTunFd` / `nativeReportStartError`, `setState(busy, connected)` for the notification,
  `shutdownService()`, `protectSocket()`, plus `nativeNetworkChanged` and `nativeSystemStart`
- UI process: Tauri WebView + Rust commands, and a `RemoteActor` (`vpn/remote.rs`) — the socket
  implementation of `TunnelControl`. `snapshot()` stays a free local read because a background task
  mirrors the state by long-polling `state_since(boot, seq)`; **unreachable is never rendered as
  disconnected**, and a `boot` that differs means the process restarted, so its state is adopted
  rather than compared against a sequence that started over
- **Lifecycle.** The UI *binds* the service on plugin load — that is what makes the process, and so
  the actor and the store, exist while the app is open, with no notification — and *starts* it
  (`ACTION_KEEP_ALIVE`) before asking for a tunnel, because a bound-only service dies with its last
  client. The service follows the actor's phase and stands down only when it settles at
  Disconnected; `backend.stop()` must never stop the service, because it runs inside every
  mid-cycle unwind. A start that is never given work stands down on a 10 s deadline
- **Consent** is asked by the UI, which has an activity; the actor only ever *checks* it. Missing
  consent is a refusal, not a waiting state: a background reconnect cannot show a dialog whatever
  it does
- **A shell is a third principal.** `AdbControlReceiver` (`:vpn`, exported behind
  `android.permission.DUMP`, which only `com.android.shell` holds) answers an ordered broadcast
  with the phase, so `just vpn-status` / `vpn-connect` / `vpn-disconnect` drive the tunnel from a
  script. `CONNECT` is `ACTION_ADB_START`, handled exactly as a system start — the service itself
  is unreachable from adb, being `exported="false"` behind `BIND_VPN_SERVICE`. The two checks a
  start with no UI must make first (consent, and something in `autostart.json` to raise) are
  `StartBlocker`, shared with the tile and the boot retry. It may also carry split rules
  (`--es split all|include|exclude --es apps a,b`, validated in Kotlin, applied through
  `nativeAdbStart` to the tunnel it starts or to one already running) — **a testing surface, not a
  setting**: the app's own rules live in the UI process's storage, so the next connect from the app
  applies those again. See `docs/ANDROID-TUNNEL-PROCESS.md`, "Driving it from a shell"
- **The system is a second principal.** A start it issues (always-on, boot, lockdown) reaches
  `nativeSystemStart`, which raises the intent from `autostart.json` — now just
  `LastIntent { order, params }`, written after every successful connect, cleared by a wipe. The
  address each host last resolved to is cached beside it (`last-endpoints.json`), because a start
  under lockdown cannot resolve anything until the tunnel is up. Android issues that start **once**
  and never retries it, and an Onyx Boox kills every EAC-enabled process ~2.5 s after boot — so
  `BootRetry.kt` (a `BOOT_COMPLETED` receiver + a 20 s `JobScheduler` job, both in `:vpn`) makes
  the same start again as `ACTION_BOOT_RETRY`, only if a genuine system start left its
  `BOOT_COUNT` marker this boot (`always_on_vpn_app` is `@hide` and throws from Android 12) and
  nothing is running. The device-side fix for the Boox itself is `just boox-eac set enable=false fullPMAccess=true`
  (`scripts/boox/OecTool.java`; see `docs/ANDROID-TUNNEL-PROCESS.md`, "Boot on an Onyx Boox")
- **Service generations** identify one request for a descriptor: `establish()` answers
  asynchronously and must name what it answers. Minted by `autostart::ServiceGenerations` (random
  per-process base, never zero); `ServiceRegistry` (`vpn/service_state.rs`, unix-gated so it is
  host-tested) holds the one being served, so a callback from an instance that has been replaced
  matches nothing
- **The network reflex.** `:vpn` follows the *underlying* network — explicitly not the default one,
  which after the tunnel comes up is our own VPN — and rebinds the tunnel's socket in place through
  gotatun's `suspend`/`resume` when it changes. It never changes what is running, so it cannot
  fight the actor's own recovery. `setUnderlyingNetworks` is set from the same callback
- IPC: tarpc over Unix socket (`vpn.sock` in app data dir), **JSON on the wire**
  (`tokio_serde::formats::Json`): every argument/return type of every `VpnRpc` method must
  round-trip through that codec in `rpc.rs` `tests::wire_coverage` (extend it when adding a method
  or a field). It was bincode until the actor moved — not self-describing, so asymmetric
  `serialize_with`/`deserialize_with`, `untagged`, internally/adjacently tagged enums, `flatten`
  and `deserialize_any` all encoded fine and failed to *decode* inside the framed transport (the
  AmneziaWG I-slots shipped broken exactly that way); JSON removes that class at the cost of one of
  its own — a non-finite `f64` is written as `null` and cannot be read back. The mirror type
  `WireConfig` existed only for bincode and is gone. Both ends are `#[cfg(unix)]`, not
  Android-only, so their tests drive a real socket on the host — and a desktop split into a
  privileged helper and a UI would reuse them rather than grow a second copy

**`tauri-plugin-vpn/`** — **Android only.** `src/android.rs` holds the implementation (`Vpn<R>`, async `VpnExt::vpn()` → `run_mobile_plugin_async` → Kotlin `@Command` methods); on other platforms `init()` registers nothing. `Error { Register, PluginInvoke }`. Kotlin side: VPN lifecycle, TUN creation, split tunneling, foreground notification, device info (`Build.MODEL`), safe area insets (`docs/SAFE-AREA-AND-Z-INDEX.md`). No iOS implementation (`docs/IOS-BACKEND-PLAN.md`).

**Key files.** `floppa-client/src-tauri/src/` is only the app's shell; every path below that starts with `vpn/` is reached through `crate::vpn`, which is `pub use floppa_vpn_core::*` plus the Tauri-only modules — so the file itself lives in **`floppa-vpn-core/src/`**. Same for `crate::provision`, which is `pub use floppa_provision::*` plus the webview's `SyncOutcome`.

In `floppa-client/src-tauri/src/` for real: `lib.rs`, `tray.rs`, `vpn/commands/`, `vpn/events.rs`, `vpn/process.rs`, `vpn/host/service.rs`, `vpn/jni_entry.rs`, `provision/mod.rs`.
- `vpn/commands/` — Tauri commands: `vpn.rs` (actor wrappers), `logs.rs` (log config + captures), and `android.rs` / `desktop.rs` — one is compiled, both define the same command names, so `lib.rs` and `bindings.ts` are platform-free
- `vpn/backend/` — `VpnBackend` trait (`mod.rs`), `in_process.rs` (desktop), `android_service.rs`
- `vpn/platform/` — `Platform` trait (`mod.rs`: routes, DNS, TUN, `default_gateway(family) -> Gateway(IpAddr)`), `linux.rs`, `windows.rs`, `android.rs`
- `vpn/actor/` — connection state machine (intents, epochs, reconcile). Vocabulary split by axis: `intent.rs`, `status.rs`, `world.rs`, `outcome.rs`, `snapshot.rs`, `policy.rs`; `types.rs` re-exports them all and is the path callers use. `vpn/rollback/` — journal of undo steps (`Step::EndpointRoute` is durable across restarts), written atomically through `vpn/private_file.rs` (as is the autostart bundle)

  Rules worth knowing before changing the actor:
  - **An adopted status owns no stack.** Adoption applied nothing, so unwinding "its" stack stops nothing: every transition out of `Up { adopted: true }` must carry `ExtraUndo::StopBackend` (`reconcile::undo_for`). Without it Down resolved and the UI said Disconnected while the tunnel was still carrying traffic, and a wipe erased the keys under it
  - **An unwind is judged by a look taken after it *finished*** (`UnwindReport::finished_at`). Step 0 of the unwind table (`U0a`/`U0b`) is therefore a safety net, not a normal path — the report shares one queue with the observations and is sent the instant the unwind ends
  - **`Intent::Down { forget }`** — an ordinary Disconnect adopts a tunnel the system restarts (row 2a); only `IntentRequest::Forget`, which `ClearConfigs` issues, stops one whoever started it (row 2b)
  - **How a cycle ends is a property of the cycle**: `Cycle::born_from_loss` (set by `Cycle::reconnect`) picks `LostGaveUp` over `Exhausted` in `Cycle::gave_up()`, used by every budget-exhaustion path in both tables. `cycle.pass` counts *burnt* passes — a cycle born from a loss has burnt none, and the view reports `pass + 1`
  - **`AttemptProgress` is not purely cosmetic**: leaving `Preparing` restarts the attempt budget once, so the desktop helper's `pkexec` prompt does not spend it
  - Both spawned tasks (attempt and unwind) have join watchers that synthesise the report they failed to send — `Unwinding` has no deadline and absorbs every intent, so a lost report is unrecoverable without one
- `vpn/state.rs` — `ProtocolConfig` = `WgConfig`/`AwgConfig` (typed `floppa_tunnel_config::TunnelConfig`, persisted through the legacy string shape via `serde(try_from/into)`) or `VlessVpnConfig`; `SavedVpnConfigs` store
- `vpn/autostart.rs` — the last-good bundle for autonomous Android starts (`TunSpec::derive` is the one derivation of TUN parameters from a config + split rules; the ladder uses it for the plugin start too)
- `vpn/config.rs` — device identity; VPN config persistence as an envelope `{updated_at, configs}`: desktop writes the OS keyring first with a 0600 file fallback (newest copy wins, file migrates back into the keyring); Android is file only (0600, private app dir). Reads the envelope and the bare `SavedVpnConfigs` payload 0.5.1 wrote (migrated on the next save); anything older is dropped with a warning and the peer is re-provisioned from the server
- `logging.rs` — `init_tracing(log_dir, LogProcess::{Ui, Vpn})`, `LogProfile::{Normal, Verbose}` + custom filter, diagnostic captures capped at 64 MiB (`docs/LOGGING.md`); `logging/capture.rs` — `CaptureSession` (Tauri state) owns start/stop/status/export of a capture under one lock

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

**Composition conventions (logic lives outside components):**
- `floppa-client/src/composables/usePeerProvisioning.ts` — the *first* provisioning of a device's peers, on mount and on retry (lookup/adopt/create per wg-family protocol, sync-vs-timeout offline banner). Server-facing parts are pure functions over an injectable `ProvisioningApi`/`ProvisioningDeps`, unit-tested with fakes; `VpnCard.vue` only renders. **Repairing a deleted peer is not here** — it is `floppa-provision/src/watcher.rs`, in the process that holds the actor, so it works with the app closed; two implementations would both replace the same peer
- `floppa-client/src/components/settings/` — `SettingsView.vue` is layout + the protocol modal; `DiagnosticsCard` (log profile/filter/capture via `composables/useLogConfig.ts`, which takes the Tauri command slice + a typed `notify` callback), `SplitTunnelingCard` (app list via `utils/appFilter.ts`, mode, dirty/reconnect), `AndroidPermissionsCard`, `AboutCard`
- `floppa-web-shared/src/components/AdminListPage.vue` — the generic (`<T>`) skeleton of every admin list (search → spinner/error → `UTable` on `md:`, cards below → pagination); views supply columns, `#<column>-cell` / `#card` slots, `@select`, and put dialogs in the default slot. Pair with `useAdminList` (search + paging) and `ConfirmModal`

**Toolchain — [Vite+](https://viteplus.dev/) (`vp`):** unified frontend toolchain (dev/build/lint/format), wraps bun. Detected via `bun.lock` + root `packageManager` field. ESLint **and** Prettier were removed in favor of `vp`'s built-in oxlint + oxfmt — config lives in the **root `vite.config.ts`** (`lint` + `fmt` blocks, globs resolved from root); per-package `vite.config.ts` files hold only Vite/framework config and import `defineConfig` from `'vite-plus'`. The `vite`/`vite-plus`/`typescript` versions are pinned via the bun `catalog:` in the root `package.json`, and the bun version via `packageManager` — CI reads both, so those two fields are the single place a toolchain version is chosen. **TypeScript stays on 6.x**: Vue's tooling does not support 7 yet, so an otherwise routine `bun update` must leave that catalog entry alone.

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

**UI:** Nuxt UI v4 via `@nuxt/ui/vite` + `@nuxt/ui/vue-plugin` (no Nuxt framework). `reka-ui` is a direct dependency of `floppa-client` (for `VisuallyHidden`/`DialogTitle`/`DialogDescription`) and is pinned to an **exact** version, never a caret: `@nuxt/ui` pins `reka-ui` exactly too, those components reach the Nuxt UI dialog through provide/inject, and a second copy breaks them silently — it also fails `vue-tsc` with two structurally identical types. Bumping `@nuxt/ui` means reading its `reka-ui` version and matching it. Components auto-imported. `useToast()` auto-imported. Icons are bundled offline: each app's `vite.config.ts` passes `icon.clientBundle.icons` = every `i-lucide-*` literal found in its sources + `floppa-web-shared/src` (`floppa-web-shared/vite/icons.ts`), so nothing is fetched from api.iconify.design at runtime; an unknown icon name fails the production build and `vite/icons.test.ts` checks the inventory against `@iconify-json/lucide`. Only literal names are collected — never build an icon name dynamically.

**Auto-generated — NEVER edit manually:**
- `floppa-web-shared/src/client/` — OpenAPI TS client. Regenerate: `just openapi`
- `floppa-client/src/bindings.ts` — tauri-specta bindings. Regenerate: `just bindings` (~1s; runs the `export_bindings` binary, no app start needed). `vp exec tauri dev` also refreshes them on startup. Commands registered in `lib.rs` via `tauri_specta::Builder`

**Data fetching:** Pinia Colada — `useQuery(getStatsQuery())`, `useMutation(createPlanMutation())` from `@pinia/colada.gen`. Each app's `main.ts` calls `installApiInterceptors(client, authStore, …)` from the shared package.

**i18n:** All locales in `floppa-web-shared/src/locales/` (en/ru). No per-app locale files. `locales.test.ts` enforces en/ru key parity.

**Tailwind v4 gotcha:** Must add `@source` in each app's CSS to scan shared components:
- `floppa-face/src/assets/main.css`: `@source "../../../floppa-web-shared/src";`
- `floppa-client/src/styles.css`: `@source "../../floppa-web-shared/src";`

Auth: Telegram Login Widget → JWT in localStorage → Bearer header. Every JWT carries a `jti` naming a `sessions` row (revocable per device or "everywhere" via `users.tokens_valid_after`); tokens issued before sessions existed have no `jti` and are accepted until they expire.
