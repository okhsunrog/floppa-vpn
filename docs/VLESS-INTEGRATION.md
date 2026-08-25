# VLESS+REALITY

How the third protocol fits into the system. For deployment (hosts, files, permissions) see
[DEPLOYMENT.md](DEPLOYMENT.md); for client-side URI handling see [VLESS-CLIENTS.md](VLESS-CLIENTS.md).

## Data model

VLESS has no peers. A user has at most one VLESS identity, `users.vless_uuid`, created on demand
(get-or-create; `floppa_core::services::ensure_vless_uuid` is the helper for it) the first time
the bot (`/vless`) or the API (`get_my_vless_config`) hands out a `vless://` URI. The `peers`
table is WireGuard/AmneziaWG only (`peers.protocol` is constrained to those two values since
migration 0014), so the daemon never sees VLESS at all and needs no protocol filter.

The URI is built by `services::generate_vless_uri` from `[vless]` in `config.toml` (endpoint,
SNI, short ID, flow) and `reality_public_key` in `secrets.toml`. Those values must match what
the proxy is configured with (below), or the REALITY handshake fails.

## The proxy (`floppa-vless`)

A separate binary built on [shoes-lite](https://github.com/okhsunrog/shoes-lite)
(VLESS+REALITY with Vision flow control). It runs on the Moscow VPS on `127.0.0.1:8444`; HAProxy
on `:443` sends every TLS connection whose SNI is not a known web host there, so the proxy is
never reachable by name.

- **Auth** — `auth.rs`: a `MultiUserAuthenticator` over an in-memory map keyed by UUID with
  constant-time comparison. It implements shoes-lite's `VlessAuthenticator` trait.
- **Registry** — `registry.rs`: loads every user with a `vless_uuid` and an active subscription
  (joined with the plan's `default_speed_limit_mbps`), then keeps the map current with
  `pg LISTEN` on `vless_user_changed` (UUID set or regenerated) and `subscription_changed`,
  a full re-sync every `traffic.sync_interval_secs` as a safety net, and exponential backoff with
  a catch-up sync when the listener connection drops.
- **Rate limits** — per-user token-bucket throttling from the plan's speed limit; there is no
  `tc` involved, the limit is applied inside the proxy.
- **Traffic** — `stats.rs`: per-user byte counters exported as Prometheus metrics on `:9103`
  every `traffic.flush_interval_secs`; VictoriaMetrics scrapes them and the server queries VM
  for the admin panel. Nothing is written back to PostgreSQL.
- **Egress** — the proxy's outbound traffic is policy-routed by service UID over the
  site-to-site tunnel to the Europe VPS and NATed there, like WireGuard and AmneziaWG.

## Config files

`/etc/floppa-vless/config.toml` (`FLOPPA_VLESS_CONFIG`):

```toml
[server]
listen_addr = "127.0.0.1:8444"

[reality]
sni = "max.ru"
short_ids = ["<hex short id>"]   # [0] must equal `[vless].short_id` in floppa-vpn config.toml
dest = "max.ru:443"

[traffic]
flush_interval_secs = 60
sync_interval_secs = 300
```

`/etc/floppa-vless/secrets.toml` (`FLOPPA_VLESS_SECRETS`):

```toml
database_url = "postgres://floppa:<password>@localhost/floppa_vpn"
reality_private_key = "<base64url x25519 private key>"
```

Both are rendered by the `floppa_vless` role in cloud-forge from `floppa_vless.*` in
`group_vars/moscow/vars.yml` and `vault_floppa_vless_reality_private_key`.
