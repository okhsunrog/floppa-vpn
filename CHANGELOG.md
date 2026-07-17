# Changelog

All notable changes to the `floppa-cli` crate are documented here. The format
is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
for the CLI crate.

## [Unreleased]

### Added
- **Auto-reconnect** (`reconnect.rs`):
  - Background watchdog that health-checks the tunnel every 30 s
    (WireGuard handshake age / VLESS TCP reachability) and rebuilds it on
    drop.
  - **Instant wake on system resume**: subscribes to systemd-logind
    `PrepareForSleep` over D-Bus (Linux) so the tunnel is rebuilt the moment
    the machine wakes from sleep — no waiting for the next watchdog tick.
  - Exponential backoff (2 s → 60 s cap) with retryable vs. fatal error
    classification; fatal errors surface so systemd `Restart=on-failure` kicks
    in.
  - `docs/RECONNECT.md` describing the mechanism and tuning knobs.
- `CONTRIBUTING.md`, `SECURITY.md`, `CHANGELOG.md` for repo hygiene.

### Changed
- `connect_wireguard` / `connect_vless` now drive the reconnect loop instead
  of blocking on a one-shot `wait_for_shutdown`. Shutdown (Ctrl+C / SIGTERM)
  still tears the tunnel down cleanly.

### Fixed
- Committed `Cargo.lock` so the `rpassword` dependency addition is
  reproducible.

## [0.2.0-cli-alpha] - 2026-07-10

### Added
- `rpassword`-based password prompt (with `FLOPPA_PASSWORD` env fallback) for
  token retrieval.
- `just` task runner targets (`build`, `lint`, `test`, `run`).

### Changed
- CLI split into the `floppa-cli` crate inside the `floppa-CLI` workspace.
