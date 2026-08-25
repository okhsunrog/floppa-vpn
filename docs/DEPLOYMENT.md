# Floppa VPN Deployment Guide

Production is deployed with Ansible from the `cloud-forge` repo (expected at `../cloud-forge/`
relative to this checkout). This document describes what gets deployed and what it needs; the
role internals live in cloud-forge.

## What runs where

| Service | Host | Binary | Runs as | Config |
|---------|------|--------|---------|--------|
| `floppa-daemon` | Moscow | `/opt/floppa-vpn/bin/floppa-daemon` | root (needs `wg`/`awg`, `tc`, `ip`) | `/etc/floppa-vpn/{config,secrets}.toml` |
| `floppa-server` | Moscow | `/opt/floppa-vpn/bin/floppa-server` | `floppa` | `/etc/floppa-vpn/{config,secrets}.toml` |
| `floppa-vless` | Moscow, behind HAProxy | `/opt/floppa-vless/bin/floppa-vless` | `floppa` | `/etc/floppa-vless/{config,secrets}.toml` |

- **floppa-daemon** — syncs WireGuard and AmneziaWG peers from PostgreSQL (`wg set` / `awg set`,
  one interface per protocol), applies per-peer HFSC rate limits, exports traffic counters on
  `:9101` for VictoriaMetrics, and runs the embedded database migrations on startup.
- **floppa-server** — Telegram bot + Axum REST API + the embedded Vue admin panel on `:3000`
  (nginx reverse-proxies the public domain to it).
- **floppa-vless** — VLESS+REALITY proxy. HAProxy on `:443` routes known web SNIs to nginx and
  everything else to `127.0.0.1:8444`, where `floppa-vless` listens. It shares the local
  PostgreSQL with the server and daemon and keeps its user registry in sync via
  `pg LISTEN/NOTIFY` (`vless_user_changed`, `subscription_changed`) plus a periodic full sync.
  Traffic counters on `:9103`.

All three read their config through environment variables set in the systemd units:
`FLOPPA_CONFIG` / `FLOPPA_SECRETS` for the daemon and server, `FLOPPA_VLESS_CONFIG` /
`FLOPPA_VLESS_SECRETS` for the proxy (defaults: `/etc/floppa-vpn/…`, `/etc/floppa-vless/…`).

The Europe VPS is the exit node: a site-to-site WireGuard tunnel from Moscow plus NAT. Nothing
from this repo runs there.

## Prerequisites

- Ansible with vault configured (`~/.vault_pass`)
- `just`, the Rust toolchain (pinned in `rust-toolchain.toml`) and Vite+ (`vp`)
- SSH access to the VPS

## 1. Generate credentials

| Secret | Generate with | Vault variable |
|--------|---------------|----------------|
| WireGuard server key | `wg genkey` | `vault_floppa_wg_private_key` |
| AmneziaWG server key (only if `[amneziawg]` is configured) | `awg genkey` (or `wg genkey` — same x25519 key format) | `vault_floppa_awg_private_key` |
| Database password | `openssl rand -base64 24` | `vault_floppa_db_password` |
| JWT secret | `openssl rand -hex 32` | `vault_floppa_jwt_secret` |
| Encryption key (WireGuard client keys at rest, 32-byte hex) | `openssl rand -hex 32` | `vault_floppa_encryption_key` |
| Telegram bot token | [@BotFather](https://t.me/BotFather) → `/newbot` | `vault_floppa_bot_token` |
| Admin Telegram IDs | [@userinfobot](https://t.me/userinfobot) | `vault_floppa_admin_telegram_ids` (list) |
| REALITY x25519 key pair (only if VLESS is deployed) | `xray x25519` or any x25519 tool, base64url | `vault_floppa_vless_reality_private_key`, `vault_floppa_vless_reality_public_key` |

Public keys for WireGuard/AmneziaWG are derived from the private keys at runtime; only the
private keys are stored. The bot's username (without `@`) goes into
`cloud-forge/group_vars/moscow/vars.yml` as `floppa_vpn.bot_username`.

```bash
cd /path/to/cloud-forge
ansible-vault edit group_vars/all/vault.yml
```

## 2. Configuration files

The Ansible templates render the same two files that `config.example.toml` and
`secrets.example.toml` document. What ends up in each:

**`config.toml`** (public settings): `min_client_version` and `allowed_origins` at the top
level, `[wireguard]` (interface, endpoint, subnet, DNS, rate limit), optional `[amneziawg]` with
`[amneziawg.rate_limit]` and `[amneziawg.obfuscation]`, `[bot]`, `[auth]`, optional `[metrics]`
(VictoriaMetrics URL) and optional `[vless]` (endpoint, SNI, short ID, flow — what the server
puts into `vless://` URIs, so it must match the proxy's REALITY settings).

**`secrets.toml`**: `database_url`, `wg_private_key`, `awg_private_key` (if AmneziaWG is
configured), `[bot] token`, `[auth] jwt_secret` / `encryption_key` / `admin_telegram_ids`, and
`[vless] reality_public_key` / `reality_private_key` if VLESS is deployed.

**`/etc/floppa-vless/config.toml`**: `[server] listen_addr`, `[reality] sni` / `short_ids` /
`dest`, `[traffic] flush_interval_secs` / `sync_interval_secs`.
**`/etc/floppa-vless/secrets.toml`**: `database_url`, `reality_private_key`.

### File permissions

`config.toml` is `0644`. For `secrets.toml` two conventions are in use, both intentional:

- **Ansible renders `secrets.toml` as `root:floppa 0640`.** `floppa-server` and `floppa-vless`
  run as the unprivileged `floppa` user and need to read it, while `floppa-daemon` runs as root;
  a group-readable file owned by root is the narrowest mode that serves both.
- **`secrets.example.toml` and the comments in `floppa-core/src/config.rs` say `0600`.** That is
  the right mode for a hand-installed or single-user setup where the reading process owns the
  file. Neither service checks the mode; pick whichever matches who runs the binaries.

## 3. Build the release archives

```bash
cd /path/to/floppa-vpn
just package         # floppa-vpn-release.tar.gz: floppa-daemon + floppa-server, migrations,
                     # systemd units, config.example.toml
just package-vless   # floppa-vless-release.tar.gz: floppa-vless + its unit
```

`just package` builds the admin panel (`floppa-face`) first; it is embedded into `floppa-server`
via `memory-serve` at compile time, so no static files are deployed. Both roles look for the
archives at `../floppa-vpn/*.tar.gz` relative to the cloud-forge checkout and fail if one is
missing. `just deploy` and `just deploy-europe` chain build and playbook.

## 4. Deploy

```bash
cd /path/to/cloud-forge
ansible-playbook site-moscow.yml --tags floppa,nginx,network
```

- `floppa` — the `floppa_vpn` role (PostgreSQL, binaries, config, units), `floppa_deploy`
  (release mirror under `/downloads/`) and `floppa_vless` (when `floppa_vless.enabled`)
- `nginx` — reverse proxy + Let's Encrypt for the public domain
- `network` — firewall ports (`51820/udp`, `51821/udp`), the site-to-site tunnel, policy routing
  and NAT for the VPN subnets

For updates, `--tags floppa` is enough.

### What the roles do

1. Install PostgreSQL, create the `floppa` role and `floppa_vpn` database
2. Create the `floppa` system user/group
3. Extract the archives to `/opt/floppa-vpn/` and `/opt/floppa-vless/`
4. Render the config and secrets files (modes above)
5. Install and enable the systemd units; migrations run when `floppa-daemon` starts

### Systemd units

The units shipped in `systemd/` and installed from the archive:

- `floppa-daemon.service` — `Requires=postgresql.service`, root, `Restart=always`
- `floppa-server.service` — `User=floppa`, `Restart=on-failure`
- `floppa-vless.service` — `User=floppa`, `Restart=always` (cloud-forge renders its own copy of
  this unit from a template rather than using the archived one)

All have `StartLimitIntervalSec=60` / `StartLimitBurst=5`, so a crash loop stops after five
restarts in a minute — `systemctl reset-failed <unit>` after fixing the cause.

## 5. Database migrations

`floppa-daemon` runs `migrations/` on startup through sqlx's embedded migrator, so a deploy that
ships a new migration applies it as soon as the daemon restarts. Three of them deserve a check on
production before the deploy that carries them:

- **0014** adds `CHECK` constraints pinning the closed value sets the code models as enums:
  `peers.sync_status` ∈ {`pending_add`, `active`, `pending_remove`, `removed`},
  `peers.protocol` ∈ {`wireguard`, `amneziawg`}, `subscriptions.source` ∈ {`trial`, `taster`,
  `purchase`, `admin_grant`}. `ALTER TABLE … ADD CONSTRAINT` validates existing rows, so a stray
  value (from a hand-written `UPDATE`, say) fails the migration and the daemon does not come up.
  Verify first:

  ```sql
  SELECT DISTINCT sync_status FROM peers;
  SELECT DISTINCT protocol    FROM peers;
  SELECT DISTINCT source      FROM subscriptions;
  ```

  Every value must be in the sets above; fix any outlier before deploying.
- **0015** drops `subscriptions.payment_id`, which nothing ever wrote or read (payments link
  the other way, via `payments.subscription_id`). No data is lost; there is no way back short
  of re-adding the column.
- **0016** adds the same kind of `CHECK` constraints for the bot's value sets:
  `users.language` ∈ {`en`, `ru`}, `telegram_link_codes.kind` ∈ {`simple`, `merge`},
  `notification_log.kind` ∈ {`expiry_1d_before`, `expiry_now`} (NULL passes for the first two).
  The migration first sets any outlier in the two nullable columns to NULL — `users.language`
  was historically copied straight from the `lang:…` callback data, which a client controls —
  but `notification_log.kind` is `NOT NULL` and left untouched, so a stray value there fails the
  migration. Verify first:

  ```sql
  SELECT DISTINCT language FROM users;
  SELECT DISTINCT kind     FROM telegram_link_codes;
  SELECT DISTINCT kind     FROM notification_log;
  ```

  Anything outside the sets above in `notification_log` must be fixed (or deleted — nothing
  reads such rows) before deploying; outliers in the other two columns are only worth a look.

## 6. Rate limits (tc)

With `rate_limit.enabled = true` the daemon owns the qdiscs on each VPN interface. Useful when
reading `tc -s class show dev wg-floppa` or debugging a limit:

- Root qdisc `1:` is HFSC on the interface (egress) and on an IFB device `ifb-<iface minus the
  wg- prefix>` (`ifb-floppa`) that receives redirected ingress; class `1:1` carries the total
  bandwidth, and the default class `1:ffff` is where unlimited peers land.
- Each limited peer gets a class whose minor id is the peer's host offset in its /16 (the last two
  octets as one 16-bit number), **rendered in hex** because that is how tc parses class ids:
  `10.100.1.5` → offset `0x0105` → class `1:105`. Offsets `0`, `1` and `0xffff` are reserved
  (network, server, default class); the daemon refuses to shape such an address.
- The peer's `u32` filter (dst IP on egress, src IP on the IFB) lives at `prio = <offset in
  decimal>` — one filter per prio, so removing a peer deletes the filter by prio alone.
- A new peer stays `pending_add` until both its WireGuard entry and its tc class are in place;
  if `tc` fails the daemon takes the just-added peer off the interface again (so it never runs
  unlimited), leaves it pending and retries on the next sync.
- Each limited peer is a separate `u32` filter at its own prio, and the kernel allows at most
  ~2046 of those per qdisc (`cls_u32` table ids are 12-bit) — a known ceiling on limited peers
  per interface, far above the current /24 subnets; see the note at the top of `tc.rs`.

## 7. Verify

```bash
ssh user@your-server "systemctl status floppa-daemon floppa-server floppa-vless"
ssh user@your-server "journalctl -u floppa-daemon -f"
ssh user@your-server "journalctl -u floppa-server -f"
ssh user@your-server "journalctl -u floppa-vless -f"
```

- Message the bot on Telegram — it should answer `/start`.
- Open `https://your-domain.example.com` and log in with Telegram.
- `ip link show wg-floppa` (and `awg-floppa` if configured), `wg show`, `awg show`.
- `curl -s 127.0.0.1:9101/metrics | head` on the server for the daemon's counters.

## Troubleshooting

- **Bot not responding** — token in vault, `journalctl -u floppa-server`, `bot_username` in
  `group_vars/moscow/vars.yml` matches the real username.
- **Peers stuck in `pending_add`** — `journalctl -u floppa-daemon`: either the `wg`/`awg` call or
  the `tc` call failed (see section 6). `awg` failing usually means the `amneziawg` kernel module
  is not loaded for the running kernel.
- **WireGuard not passing traffic** — firewall (`51820/udp`, `51821/udp`), the site-to-site tunnel
  to Europe and the policy routes belong to the `network` tag.
- **Daemon fails to start after a deploy** — check the migration output in its journal; see
  section 5 for the constraint migrations.
- **VLESS handshake fails** — `[vless]` in `config.toml` (SNI, short ID) must match `[reality]` in
  `/etc/floppa-vless/config.toml`, and `reality_public_key` in `secrets.toml` must be the public
  half of the proxy's `reality_private_key`.
- **Admin panel not loading** — `journalctl -u floppa-server`, `nginx -t`, the certificate for the
  domain.
