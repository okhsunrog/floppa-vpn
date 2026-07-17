# Security Policy

## Scope

floppa-cli is the client-side connector for the floppa-vpn stack. It runs
with elevated privileges (it manipulates network interfaces, routes and
`/etc/resolv.conf`), so the security bar is high.

In scope:
- The `floppa-cli` binary crate and its reconnect / network-plumbing code.
- How it handles configs, tokens and DNS.

Out of scope (handled in the parent `floppa-vpn` repo):
- The server, the Telegram bot, the admin panel, the VLESS proxy.

## Private data handling

- **Configs are supplied at runtime** (file path, stdin, or API URL). They are
  never committed to the repository.
- **Auth tokens** fetched from the API are saved to a user-owned file
  (`~/.config/floppa-cli/token` or `XDG_CONFIG_HOME` equivalent), `0600`.
  They are not logged.
- **Passwords** for token retrieval are read from the terminal with no echo
  (`rpassword`) or from the `FLOPPA_PASSWORD` environment variable. The env
  var is the non-interactive escape hatch; don't commit it.
- **DNS**: floppa-cli rewrites `resolv.conf` while connected and restores the
  previous contents on disconnect. It does not exfiltrate DNS state.

## What we consider a vulnerability

- Privilege escalation or running more than necessary as root.
- Leaking tokens / passwords / config contents into logs, crash dumps, or
  process listings.
- A way to make the client connect to an attacker-controlled endpoint without
  user consent (config injection).
- The auto-reconnect loop being abuseable to wedge the host network.

## Reporting

Please **do not** open a public issue for security problems.

- Email: engineerutron@gmail.com
- Or use GitHub's private vulnerability reporting on the repo.

We aim to acknowledge within 72 hours and to ship a fix or mitigation within
14 days for anything rated High or Critical.

## Build / supply-chain notes

- `Cargo.lock` is committed; builds are reproducible and dependency versions
  are pinned.
- `gotatun` and `shoes-lite` are pulled from git `rev`s pinned in
  `Cargo.toml` — review lockfile diffs on dependency bumps.
- The binary shells out to system tools (`ip`, `wg`/`awg`, `resolvectl`/
  `resolvconf`) for network plumbing; those run with the privileges of the
  process. Keep the set minimal.
