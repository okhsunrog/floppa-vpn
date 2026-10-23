# AGENTS.md — Working Effectively With This Template

This document is a concise playbook for AI agents (and humans) developing apps with this Tauri + Vue template. It covers the project layout, core workflows, and “gotchas” to keep changes safe, consistent, and shippable.

## TL;DR Commands (Vite+)

- Install: `vp install` (uses the pinned Bun version for this workspace)
- Dev: `vp exec tauri dev` (runs Vite + Tauri, generates Specta bindings, streams logs)
- Build: `vp exec tauri build` (packs installers/bundles)
- Lint/Format/Types:
  - `vp lint`, `vp fmt`, `vp check`
  - `vp run typecheck` for the Vue-specific `vue-tsc` pass

Use Vite+ for runtime, package management, scripts, and frontend tools. It delegates package
operations to the pinned Bun version; do not invoke npm/yarn/pnpm directly.

## Architecture Overview

- Frontend (Vue 3 + Vite + TS)
  - State: Pinia with persisted state
  - UI styling: Tailwind v4 + DaisyUI v5 (themes: `nord` and `dim`)
  - Terminal: xterm.js component (`src/components/terminal/TerminalDisplay.vue`) that listens to Rust logs
  - Theme toggle: `src/components/common/ThemeToggle.vue` (DaisyUI Theme Controller)
  - Specta bindings: `src/bindings.ts` (generated during dev)

- Backend (Tauri 2 + Rust)
  - `src-tauri/src/lib.rs`: builds the Tauri app, registers Specta commands/events, initializes plugins
  - `src-tauri/src/commands/mod.rs`: example commands (`greet`, `emit_test_logs`) annotated for Specta
  - `src-tauri/src/logging.rs`: tracing subscriber that forwards logs as `log-event` to the frontend
  - Plugins enabled: dialog, fs, log, os
  - Capabilities (ACL): `src-tauri/capabilities/default.json`
  - Config (JSON5): `src-tauri/tauri.conf.json5` (icons, dev hooks, android block)

## Adding a New Rust Command (with Specta)

1) Implement in `src-tauri/src/commands/mod.rs`:

```rust
#[tauri::command]
#[specta::specta]
pub async fn do_something(arg: String) -> Result<String, String> {
    tracing::info!("do_something called: {}", arg);
    Ok(format!("processed: {}", arg))
}
```

2) Register it in `src-tauri/src/lib.rs`:

```rust
let specta_builder = tauri_specta::Builder::<tauri::Wry>::new()
    .commands(tauri_specta::collect_commands![
        commands::greet,
        commands::emit_test_logs,
        commands::do_something,
    ])
    .events(tauri_specta::collect_events![logging::LogEvent]);
```

3) Run `vp exec tauri dev` to regenerate `src/bindings.ts` and use it on the frontend via `import { commands } from '@/bindings'` (or call via `invoke('do_something', { arg })`).

## Adding an Event

1) Define a serializable type and derive Specta Event in Rust:

```rust
#[derive(Debug, Clone, serde::Serialize, specta::Type, tauri_specta::Event)]
pub struct MyEvent { pub message: String }
```

2) Add it to `.events(collect_events![...])` in `lib.rs` and emit via `app_handle.emit("my-event", MyEvent { ... })`.

3) After `vp exec tauri dev`, consume in FE via Specta bindings: `events.myEvent.listen(cb)`.

## Logging → Terminal

- Use `tracing` macros (`trace!`, `debug!`, `info!`, `warn!`, `error!`).
- Logs are forwarded to the frontend as `log-event` (see `logging.rs`).
- Terminal colors are aligned with DaisyUI v5 (themes `nord`/`dim`).
- There is a sample button in the UI to emit test logs.

## Permissions & Capabilities

- Edit `src-tauri/capabilities/default.json` to grant only what’s needed (dialog/fs/log/os are enabled by default here). Keep least-privilege in mind.
- If you add a new plugin, mirror JS and Rust versions and extend capabilities accordingly.

## Theming (DaisyUI v5)

- Themes: `nord` (light), `dim` (dark). Default is applied pre-mount (see `index.html`).
- The Vite plugin injects DaisyUI with the allowed themes based on `src/constants/themes.ts`.
- Terminal theme recalculates from CSS variables on theme changes (MutationObserver).
- Latest DaisyUI docs for LLMs: https://daisyui.com/llms.txt

## Version Compatibility (Important)

- Use the stable Tauri 2.12 release line for Rust, the JS API, and the CLI.
- Tauri 2.12 includes the Android activity lifecycle fix and the newer Tao Wayland input fix;
  the temporary Tauri Git patches are no longer needed. Keep the Foldfin HTTP/3 patches.
- Keep plugin versions aligned between NPM and Rust crates (same major/minor). Examples:
  - `@tauri-apps/plugin-fs` v2.4.x ↔ `tauri-plugin-fs` 2.4.x
  - `@tauri-apps/plugin-os` v2.3.x ↔ `tauri-plugin-os` 2.3.x
  - `@tauri-apps/plugin-dialog` v2.4.x ↔ `tauri-plugin-dialog` 2.4.x

## Script Reference (Vite+)

- `vp install` – Install dependencies with the pinned Bun package manager
- `vp exec tauri dev` – Dev (Vite + Tauri, generates Specta bindings)
- `vp exec tauri build` – Build installers/bundles
- `vp run typecheck` – cached `vue-tsc` project check
- `vp lint` / `vp lint --fix` – Oxlint
- `vp fmt` / `vp fmt --check` – Oxfmt
- `vp check` – Format, type-aware lint, and TypeScript checks

## Security Notes

- Current CSP is `null` to ease development; consider tightening for production.
- FS plugin permissions include `fs:allow-write-text-file`; restrict as needed.

## When Extending the Template

- Prefer adding new commands under `src-tauri/src/commands/` and registering in `lib.rs`.
- Regenerate bindings by running the dev task after changes.
- Keep UI theming in sync with DaisyUI variables; extend `themes.ts` if adding themes.
- Avoid mixing package managers; use Bun only.
- Do not introduce i18n unless explicitly requested; if needed, follow the `fwupd-gui` pattern (vue-i18n + unplugin-vue-i18n) and wire it in `vite.config.ts`.

## Minimal Validation Checklist

1) `vp exec tauri dev` launches (no version mismatch errors)
2) Theme loads as `nord` by default; toggle to `dim` works
3) Terminal displays startup logs and test logs
4) `vp check` and `vp run typecheck` pass
5) `vp exec tauri build` succeeds

## Android ABI Support

- The published APK contains both `armeabi-v7a` (ARM32) and `arm64-v8a` (ARM64) native libraries.
- Build the universal APK with `just build-android`; it writes `app-universal-release.apk`.
- Android Rust checks in CI cover both ABIs with `cargo-ndk`.
- Rust `File::try_lock` is unsupported on Android in the pinned toolchain. Tunnel
  ownership uses `libc::flock` on Linux and Android; do not replace it with the std API.
- Before publishing an Android release, install the signed APK on a device and verify
  cold start, connect, disconnect, and restart with logcat recording. Cross-compilation
  and Clippy alone do not validate Android runtime support.

## Linux network helper

`floppa-network-helper` is a separate Rust workspace binary. Native Linux bundles
must first build `cargo build --release --locked -p floppa -p floppa-network-helper`;
the deb/rpm file maps install the helper and polkit policy. Nix builds it separately
with a fixed tool search path. Never restore an embedded Bash helper fallback.
For standalone GUI source development, install the helper explicitly with
`sudo target/debug/floppa-network-helper install` after building it without test
features. Root CLI source runs can use a helper beside the CLI executable.
`test-hooks` binaries are exclusively for private mount/network namespace tests;
never install or package them. See the Linux recovery section in
`docs/CONNECTION-ARCHITECTURE.md` before changing privileged operations.
