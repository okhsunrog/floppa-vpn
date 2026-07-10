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
- [x] `floppa-vless/` crate with its own config (runs on Moscow behind HAProxy)
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
- [x] Generate REALITY x25519 keypair and add it to the Ansible vault
- [x] Deploy `floppa-vless`, config, secrets, and systemd service on Moscow
- [x] Route non-web TLS from Moscow HAProxy :443 to `127.0.0.1:8444`
- [x] Route `floppa-vless` outbound traffic through the Moscow–Europe tunnel by service UID
- [x] NAT VLESS egress on Europe with the other VPN traffic
- [x] Run `cargo sqlx prepare --workspace` to maintain the offline query cache

## Architecture

```
Client (Android/Desktop)
  └─ vless:// URI
      └─ VLESS+REALITY+Vision connection
          └─ Moscow HAProxy :443
              └─ local floppa-vless (127.0.0.1:8444)
                  ├─ REALITY handshake (camouflage: max.ru)
                  ├─ VLESS UUID auth (multi-user, from PostgreSQL)
                  ├─ Vision flow control (zero-copy TLS-in-TLS)
                  └─ Proxy via wg1 → Europe NAT → internet

Moscow VPS (floppa-server + floppa-daemon + floppa-vless)
  ├─ API: creates VLESS peers, generates vless:// URIs
  ├─ PostgreSQL: stores peers, subscriptions, plans
  └─ WireGuard: existing WG peers (unaffected)

DB sync: local PostgreSQL on Moscow
  ├─ LISTEN/NOTIFY: real-time peer/subscription changes
  └─ Periodic full sync: safety net every 5 min
```

## Config Files

### Moscow VPS: `/etc/floppa-vless/config.toml`
```toml
[server]
listen_addr = "127.0.0.1:8444"

[reality]
sni = "max.ru"
short_ids = ["<configured short ID>"]
dest = "max.ru:443"

[traffic]
flush_interval_secs = 60
sync_interval_secs = 300
```

### Moscow VPS: `/etc/floppa-vless/secrets.toml`
```toml
database_url = "postgres://floppa:<password>@localhost/floppa_vpn"
reality_private_key = "<base64url x25519 private key>"
```
