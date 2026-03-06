# VLESS+REALITY Integration Status

## What's Done

### Phase A: Backend + DB
- [x] DB migration: `protocol`, `vless_uuid` columns, nullable `public_key`/`assigned_ip`
- [x] `floppa-core/models.rs`: Peer struct updated with protocol + vless_uuid fields
- [x] `floppa-core/config.rs`: `VlessConfig`, `VlessSecrets` structs
- [x] `floppa-core/services.rs`: `create_peer()` handles both WireGuard and VLESS, `generate_vless_uri()`, `CreatePeerContext` struct
- [x] `floppa-server` API: protocol in request/response for create, list, get-config, send-config, by-device endpoints
- [x] `floppa-daemon/sync.rs`: all queries filtered with `AND protocol = 'wireguard'` so daemon ignores VLESS peers

### Phase B: floppa-vless Server Binary
- [x] `crates/floppa-vless/` crate with own config (separate from floppa-core, runs on EU VPS)
- [x] Multi-user UUID auth: `VlessAuthenticator` trait in shoes-lite, `MultiUserAuthenticator` with DashMap + constant-time comparison
- [x] REALITY+Vision server handler chain using shoes-lite
- [x] UUID registry with PostgreSQL LISTEN/NOTIFY (real-time sync on `peer_changed` / `subscription_changed`)
- [x] Periodic full sync as safety net (configurable interval)
- [x] Exponential backoff on listener disconnection with catch-up sync
- [x] Traffic stats module (flush to DB periodically)
- [x] Graceful shutdown (SIGINT handler, final stats flush)

### shoes-lite Changes
- [x] `VlessAuthenticator` trait + `SingleUserAuthenticator` for backwards compat
- [x] `VisionVlessConfig` uses `Arc<dyn VlessAuthenticator>` instead of single `user_id`
- [x] Internal modules made public for external server usage

## What Remains

### Phase B (incremental improvements)
- [ ] Per-connection traffic recording: wrap streams in a byte-counting adapter so `tx_bytes`/`rx_bytes` are updated in the DB
- [ ] Per-user rate limiting via `async-speed-limit`: throttle bandwidth based on subscription plan's `speed_limit_mbps`

### Phase C: Client Updates
- [ ] Regenerate OpenAPI spec + `types.gen.ts` with protocol fields
- [ ] Protocol selection UI in VpnCard.vue (switch between WireGuard and VLESS)
- [ ] Update `doServerSync()` to use server-provided protocol field instead of auto-detecting by string prefix
- [ ] Protocol switch flow: delete old peer, create new with selected protocol

### Phase D: Deployment
- [ ] Generate REALITY x25519 keypair, add to Ansible vault
- [ ] Moscow: update `config.toml` with `[vless]` section, `secrets.toml` with VLESS keys
- [ ] Moscow: allow PostgreSQL from EU tunnel IP (`10.77.77.1`) in `pg_hba.conf` and firewall
- [ ] EU VPS: create `floppa-vless` Ansible role (binary + config + systemd service)
- [ ] EU VPS: deploy `config.toml` and `secrets.toml` to `/etc/floppa-vless/`
- [ ] EU VPS: open port 443 TCP in firewall
- [ ] Run `cargo sqlx prepare --workspace` to update offline query cache before cross-compiling

## Architecture

```
Client (Android/Desktop)
  └─ vless:// URI
      └─ VLESS+REALITY+Vision connection
          └─ EU VPS (floppa-vless :443)
              ├─ REALITY handshake (camouflage: www.microsoft.com)
              ├─ VLESS UUID auth (multi-user, from PostgreSQL)
              ├─ Vision flow control (zero-copy TLS-in-TLS)
              └─ Proxy to internet

Moscow VPS (floppa-server + floppa-daemon)
  ├─ API: creates VLESS peers, generates vless:// URIs
  ├─ PostgreSQL: stores peers, subscriptions, plans
  └─ WireGuard: existing WG peers (unaffected)

DB sync: EU ↔ Moscow via wg1 tunnel (10.77.77.0/24)
  ├─ LISTEN/NOTIFY: real-time peer/subscription changes
  └─ Periodic full sync: safety net every 5 min
```

## Config Files

### EU VPS: `/etc/floppa-vless/config.toml`
```toml
[server]
listen_addr = "0.0.0.0:443"

[reality]
sni = "www.microsoft.com"
short_ids = ["abcdef1234567890"]
dest = "www.microsoft.com:443"

[traffic]
flush_interval_secs = 60
sync_interval_secs = 300
```

### EU VPS: `/etc/floppa-vless/secrets.toml`
```toml
database_url = "postgres://floppa:<password>@10.77.77.2/floppa_vpn"
reality_private_key = "<base64url x25519 private key>"
```
