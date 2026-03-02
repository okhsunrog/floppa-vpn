# Tauri + Vue 3 Template (with Specta, Themes, Terminal)

An opinionated Tauri 2 + Vue 3 template that mirrors a production setup:

- Vue 3 + Vite 6 + TypeScript 5
- Pinia + persisted state
- Tailwind CSS v4 + DaisyUI v5 with matching terminal theme (nord/dim)
- Integrated xterm.js terminal streaming Rust logs to the UI
- Tauri 2 plugins (dialog, fs, log, os) with capabilities configured
- Specta + tauri-specta for typed commands/events and generated bindings
- ESLint (flat) + Prettier + TS project references

## Features

- Theming:
  - DaisyUI themes: `nord` (light), `dim` (dark)
  - Theme toggle (DaisyUI Theme Controller with icons)
  - Default theme applied before the app mounts to avoid flashes
- Terminal:
  - xterm.js viewer with color palette derived from DaisyUI CSS variables
  - Listens to Rust tracing logs via a Tauri event (`log-event`)
  - “Emit Test Logs” button for quick visual validation
- Tauri integration:
  - Plugins: dialog, fs, log, os
  - Capabilities set in `src-tauri/capabilities/default.json`
  - JSON5 config (`src-tauri/tauri.conf.json5`) with extended icons
- Specta bindings:
  - Commands and events registered in Rust
  - TypeScript bindings generated to `src/bindings.ts` in dev builds

## Recommended IDE Setup

- VS Code + Volar extension (disable Vetur)
- Enable ESLint and Prettier extensions

## Requirements

- Bun (package manager and script runner). Install from https://bun.sh
- Rust toolchain (stable), plus platform-specific Tauri deps

## Install (Bun)

```sh
bun install
```

## Run (development)

```sh
bun tauri dev
```

This starts Vite and the Tauri shell, generates/refreshes `src/bindings.ts` for Specta in debug mode, and streams logs to the terminal panel.

## Build (production)

```sh
bun tauri build
```

## Scripts (Bun)

- `bun install` – Install dependencies
- `bun tauri dev` – Full app dev (Vite + Tauri)
- `bun tauri build` – Build installers/bundles
- `bun type-check` – `vue-tsc` project check
- `bun lint` – ESLint with fixes
- `bun lint:check` – ESLint check only
- `bun format` – Prettier write
- `bun format:check` – Prettier check

## Project Structure

- `src/components/common/ThemeToggle.vue` – DaisyUI theme controller (nord/dim)
- `src/components/terminal/TerminalDisplay.vue` – xterm terminal bound to logs
- `src/stores/terminalStore.ts` – Pinia store that listens to `log-event` and forwards `console.*` to plugin-log
- `src/constants/themes.ts` – Theme constants and validator used by injector and UI
- `vite.config.ts` – Vue plugin, DaisyUI theme injector, Vue DevTools (dev only), `base: './'`
- `src-tauri/src/logging.rs` – Tracing subscriber that emits logs to the frontend
- `src-tauri/src/commands/mod.rs` – Example commands (`greet`, `emit_test_logs`)
- `src-tauri/src/lib.rs` – Tauri builder, plugin init, Specta registration
- `src-tauri/tauri.conf.json5` – App config (JSON5), icons, dev/build hooks
- `src-tauri/capabilities/default.json` – Capability ACLs for plugins

## Theming Details (DaisyUI v5)

- The theme injector (Vite plugin) injects DaisyUI with the `nord` + `dim` themes.
- `index.html` applies the saved theme before mount to prevent a flash.
- Terminal theme is computed from DaisyUI CSS variables and updates on theme changes.

## Tauri Plugins & Capabilities

Enabled plugins (JS + Rust):

- `@tauri-apps/plugin-dialog` / `tauri-plugin-dialog`
- `@tauri-apps/plugin-fs` / `tauri-plugin-fs`
- `@tauri-apps/plugin-log` / `tauri-plugin-log`
- `@tauri-apps/plugin-os` / `tauri-plugin-os`

Capabilities (`src-tauri/capabilities/default.json`) allow default use and file write.

## License

This template is provided as-is. You may adapt it for your projects.
