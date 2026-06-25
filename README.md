# Floppa VPN CLI

<p align="center">
  <img src="branding/logo-solid.png" alt="Floppa VPN logo" width="180" />
</p>

CLI-only fork/branch for the headless `floppa-cli` utility. This branch intentionally does not include the Tauri desktop client, web admin UI, server, daemon, migrations, mobile platform glue, or packaging assets from the upstream monorepo.

## What is included

- `floppa-cli` Rust binary.
- Root Cargo workspace and lockfile for reproducible CLI builds.
- Linux CLI networking code for WireGuard/AmneziaWG and VLESS+REALITY tunnels.
- Telegram and account login/password auth flow used by the CLI.
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
- Built-in safe tunnel stop command:
  - `floppa-cli stop`
  - `floppa-cli stop --interface floppa0`
  - `floppa-cli stop --pid <pid>`
  - `floppa-cli stop --force`
- Systemd service management for headless Linux hosts:
  - `floppa-cli service install`
  - `floppa-cli service start`
  - `floppa-cli service stop`
  - `floppa-cli service restart`
  - `floppa-cli service status`
  - `floppa-cli service enable`
  - `floppa-cli service disable`
  - `floppa-cli service uninstall`
- Idempotent Linux route handling with `ip route replace`.
- Cleanup guard for DNS and routes on Ctrl+C/SIGTERM/error paths.
- Basic route/interface verification after tunnel setup.

## Build, test, and CI

Local checks:

```bash
./scripts/smoke-test.sh
```

The smoke script runs:

- `cargo fmt --check`
- `cargo test -p floppa-cli --locked`
- `cargo clippy -p floppa-cli --locked -- -D warnings`
- `cargo check -p floppa-cli --locked`
- `cargo build --release --locked -p floppa-cli`
- `./target/release/floppa-cli --help`
- `./target/release/floppa-cli --version`
- `./target/release/floppa-cli status` without failing when no tunnel is active

Optional install smoke test:

```bash
RUN_CARGO_INSTALL=1 ./scripts/smoke-test.sh
```

GitHub Actions CI is defined in:

```text
.github/workflows/ci.yml
```

It runs the same smoke script on `main` and pull requests.

Release artifacts are built by:

```text
.github/workflows/release.yml
```

The release workflow triggers on `v*` tags and creates a draft GitHub Release with Linux, Windows, and macOS binaries plus `SHA256SUMS.txt`.

## Release verification

Release verification for `v0.1.0-cli-alpha` was performed before publishing:

- `./scripts/smoke-test.sh` passed locally.
- GitHub Actions CI was green on `main`.
- `./target/release/floppa-cli --help` worked.
- `./target/release/floppa-cli --version` worked.
- `./target/release/floppa-cli status` worked without an active tunnel.
- `./target/release/floppa-cli stop` worked without an active tunnel.
- Release workflow created a draft GitHub Release on the test `v0.1.0-cli-alpha-test` tag.
- Linux, Windows, and macOS artifacts were attached to the draft release.
- `SHA256SUMS.txt` was attached to the draft release.
- The test tag and test draft release were deleted after verification.
- Final release workflow run `27792844338` passed on `v0.1.0-cli-alpha`.
- Downloaded release assets were verified with `SHA256SUMS.txt`.

After the test release was verified, the final `v0.1.0-cli-alpha` tag can be published from the GitHub draft release.

## Install

Build and install the CLI binary:

```bash
cargo build --release -p floppa-cli
install -m 0755 target/release/floppa-cli "$HOME/.local/bin/floppa-cli"
```

Or install from the local crate path:

```bash
cargo install --path floppa-cli --locked
```

After installation, `~/.local/bin` should be in your shell PATH. For privileged network changes, use the absolute binary path because `sudo secure_path` may not include `~/.local/bin`:

```bash
sudo env HOME="$HOME" "$HOME/.local/bin/floppa-cli" status
sudo env HOME="$HOME" "$HOME/.local/bin/floppa-cli" stop
```

## Run

```bash
cargo run -p floppa-cli -- --help
cargo run -p floppa-cli -- login
cargo run -p floppa-cli -- login --method account --login your-login
cargo run -p floppa-cli -- login-account --login your-login
cargo run -p floppa-cli -- connect --protocol amneziawg
cargo run -p floppa-cli -- status
cargo run -p floppa-cli -- stop
```

`login` prompts for the login method (`telegram` or `account`). Use `--method account --login your-login` to skip the prompt and authenticate with Floppa account credentials. `login-account` remains available as a direct account-login command. By default, the password is prompted without echoing it; for automation, set `FLOPPA_ACCOUNT_LOGIN` and `FLOPPA_ACCOUNT_PASSWORD` instead of passing secrets on the command line.

Installed binary examples:

```bash
floppa-cli login
floppa-cli login --method account --login your-login
floppa-cli login-account --login your-login
floppa-cli device show
floppa-cli peer delete --protocol amneziawg
floppa-cli connect --protocol amneziawg --no-dns
floppa-cli status
floppa-cli stop
```

Privileged run examples:

```bash
sudo env HOME="$HOME" "$HOME/.local/bin/floppa-cli" connect --protocol amneziawg --no-dns
sudo env HOME="$HOME" "$HOME/.local/bin/floppa-cli" status
sudo env HOME="$HOME" "$HOME/.local/bin/floppa-cli" stop
```

## Systemd service

`floppa-cli service` installs and manages a systemd unit that runs `floppa-cli connect` in the background. The default scope is `system`, so `install`, `uninstall`, `enable`, and `disable` use `sudo systemctl`; `status`, `start`, `stop`, and `restart` also use `sudo systemctl` by default.

Install a system service using the currently running binary:

```bash
floppa-cli service install --scope system --name floppa-cli --protocol amneziawg --no-dns
sudo systemctl enable --now floppa-cli
```

Useful service commands:

```bash
floppa-cli service status
floppa-cli service start
floppa-cli service stop
floppa-cli service restart
floppa-cli service enable
floppa-cli service disable
floppa-cli service uninstall
```

For user services, use `--scope user`; the unit is written under `$XDG_CONFIG_HOME/systemd/user` and managed with `systemctl --user`.

The generated unit runs the service as the current user, sets `HOME`, grants `CAP_NET_ADMIN` and `CAP_NET_RAW` through systemd capabilities, restarts on failure, and logs to the journal. The default log file is `~/.local/state/floppa-cli/floppa-cli.log`; override it with `--service-log-file /absolute/path.log`.

Connecting to a real VPN requires the user's Floppa account/session and Linux network privileges. Do not commit private configs, tokens, VPN keys, or user-specific absolute paths.

## Why this branch exists

The upstream repository contains the full cross-platform product: desktop client, web UI, server, daemon, mobile platform code, packaging, and integration tests. For CLI hardening and headless use cases, this branch keeps only the code needed to build, test, and run `floppa-cli`.
