# Contributing

Thanks for looking at floppa-cli! This document covers the bits that are
specific to this crate. For the wider project see the parent `floppa-vpn`
workspace.

## Repository layout

```
floppa-CLI/                 workspace root (this repo)
├── floppa-cli/             the CLI binary crate (where the logic lives)
│   ├── src/
│   │   ├── main.rs         CLI, config resolution, connect_* entry points
│   │   ├── tunnel.rs       WireGuard / AmneziaWG setup (gotatun)
│   │   ├── vless.rs        VLESS+REALITY setup (shoes-lite)
│   │   ├── reconnect.rs    auto-reconnect loop + DBus sleep/resume watcher
│   │   ├── net.rs          policy routing / interface plumbing
│   │   ├── dns.rs          resolv.conf management
│   │   ├── service.rs      systemd unit rendering
│   │   └── ...
│   └── Cargo.toml
├── docs/                   design + ops docs (incl. RECONNECT.md)
├── systemd/                example unit files
└── justfile                common dev tasks
```

## Getting started

```bash
# Build (needs network plumbing deps; runs as root to actually connect)
just build            # or: cargo build -p floppa-cli

# Lint + test
just lint             # cargo clippy -- -D warnings
just test             # cargo test

# Run against a local test setup (see docs/LOCAL-VPN-TESTING.md)
sudo cargo run -p floppa-cli -- <config> --no-dns
```

`just` is the task runner — `just --list` shows every target.

## Before you open a PR

- `cargo clippy -- -D warnings` is clean (CI enforces this).
- `cargo test` passes.
- `Cargo.lock` is committed — keep builds reproducible. If you changed
  dependencies, commit the refreshed lockfile.
- New behaviour in `reconnect.rs` should come with a unit test where it makes
  sense (backoff math, signal plumbing, retryability).
- Keep credentials/secrets out of the tree. Configs are passed at runtime,
  never committed.

## Commit / PR style

- Small, focused commits. One logical change per commit.
- Write the *why*, not the *what* — the diff already shows the what.
- PRs target the `cli-upstream-sync` branch (the sync fork's integration
  branch), not `main`, unless you know what you're doing.

## Code notes

- The reconnect loop owns the tunnel lifecycle. `connect_wireguard` /
  `connect_vless` build `rebuild` / `health` closures and hand them to
  `reconnect::run`. Don't block the loop on a `RefCell` borrow across an
  `.await` (clippy `await_holding_refcell_ref`) — take the value out of the
  shared cell first.
- `gotatun::Device` and `shoes_lite::VlessTunnel` are **not** `Clone` and are
  torn down via `stop(self)`. Rebuild creates a fresh one each time.
