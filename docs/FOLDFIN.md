# Foldfin integration and access design

The client now imports Foldfin JSON into the existing config store, selects it as
`Protocol::Foldfin`, and runs the shared engine through `TunnelManager` and the
connection actor. Routes, DNS, rollback, reconnection and platform consent remain
owned by Floppa. No extra VPN process or reconnect loop is introduced.

Linux opens the persistent TUN prepared by the existing helper, applies MTU before
routes, and marks outer TCP before connect when permitted. Android protects TCP
before connect, aborts on protection failure, and gives Foldfin an owned CLOEXEC
duplicate of the nonblocking Kotlin descriptor. SNI and HTTP authority are kept
separate from the resolved dial address. Foldfin is not implemented on Windows or
macOS yet. Host Rustls and Android builds use certificate/name verification;
Chrome's measured Linux profile is optional via `foldfin-chrome`.

## Import and run

Development dependency: `floppa-vpn-core` and `floppa-foldfin` currently use the
local checkout at `../../rust/foldfin`. Keep `floppa-vpn` in `~/code/floppa-vpn` and
Foldfin in `~/code/rust/foldfin`. Release packaging/CI needs a published or pinned
Git Foldfin dependency before distribution; the current local path is intentional.

Import this document through the existing app import action or
`floppa import <file>`. For a one-off privileged desktop run, use
`sudo floppa connect --config <file> --no-dns`. Store the document privately:
it contains a device bearer credential. The token below is a dummy example.

```json
{
  "protocol": "foldfin",
  "config": {
    "version": 1,
    "endpoint": "vpn.example.org:8445",
    "server_name": "vpn.example.org",
    "token": "0123456789abcdef0123456789abcdef",
    "address": "198.18.77.2/32",
    "peer": "198.18.77.1",
    "routes": ["198.18.77.1/32", "198.19.77.0/24"],
    "dns": [],
    "mtu": 1280,
    "chrome_tls": false
  }
}
```

Optional `additional_ca_pem` embeds a development CA; production normally omits
it and uses the system profile's public trust anchors. The current document holds
one IP family, matching the actor's single address setup. Routes must be canonical
and match that family. MTU is 1280..1500. Arbitrary host/LAN routes in the GUI do
not grant access: the server's grant must permit them as well. Full internet
routing requires a server grant, forwarding and egress setup; importing `0.0.0.0/0`
alone does not configure server NAT or firewall.

The CLI does not auto-fetch Foldfin credentials yet. A Foldfin API provisioning
request reports this explicitly instead of fetching a VLESS URI. The WG peer
repair watcher does not repair Foldfin as a WireGuard peer.

## Implemented server boundary

`floppa-foldfin::DeviceRegistry` implements Foldfin's `Authorizer`. The host
installs trusted `DeviceGrant`s containing a device/region key, a token digest,
expiry, proxy rights, and optional VPN policy/device factory. `registry.server(tls)`
constructs the listener engine; `Server::serve(listener, shutdown)` owns and joins
its connection tasks. The registry does not read PostgreSQL or configure the OS.

Every CONNECT is authorized, including new flows on an existing H2 connection.
Wrong credentials and disallowed modes fail closed. Rotating/replacing a device
grant or `revoke(device_id)` cancels its active streams. Expiry cancels streams
without a periodic sweep. Dropping the registry's revocation sender also cancels
access. Tokens are kept as SHA-256 digests, not plaintext. A digest collision across
device keys is rejected. Device IDs are control-plane keys, not authentication by
themselves; they must come from an ownership-checked database row.

The grant has one owned packet session per device. A new authenticated CONNECT-IP
cancels the previous device session and waits for its packet ownership to be
released before admitting the successor. This supports a new carrier after roaming
without waiting for the old TCP socket to time out. Proxy flows use a separate
lifetime and are not cancelled just because a VPN session is replaced. The device
VPN slot is shared across credential rotation/revocation, so old and new grants
cannot concurrently open the same packet device. Factories must return isolated packet IO;
multiple `NamedTun` readers on a shared interface would steal each other's packets.
The shared TUN owner/router and node grant-sync service are still required before
multi-user deployment. Integration tests use isolated in-memory endpoints. The
legacy Frankfurt CLI server continues using its existing static token and peer.

## Proposed authentication and authorization

Use existing Floppa account authentication only for the control API. Issue a
separate opaque VPN credential for each installation, scoped to Foldfin and a
region (or an explicit allowed region set). Generate 32 random bytes and encode
as 64 hex characters, compatible with the current credential API. Never reuse the
account JWT, Telegram identity, VLESS UUID or a node-wide shared token as this
credential. Bearer credentials require validated TLS and private client storage;
possession is sufficient, so a copied credential is usable until revoked.
See [RFC 6750](https://www.rfc-editor.org/rfc/rfc6750.html).

The provisioning transaction must:

1. Authenticate the existing API session and load its owning user.
2. Verify active subscription, installation ownership, device quota and region entitlement.
3. Allocate a stable tunnel address and device grant; check address/prefix ownership.
4. Store the credential digest and grant version with the installation, subscription
   source and region. Return the credential once with the versioned client document.
5. Deliver committed grants to nodes; update installation/config state using the
   existing provision/store boundary. A successful actor connection remains distinct
   from configuration import.

Do not put token plaintext in PostgreSQL, logs, config summaries or RPC status.
Recovery should rotate a credential, rather than retrieve its plaintext from the
server. High-entropy random credentials can use SHA-256; human passwords continue
using the existing password KDF. Credential IDs, hashes and internal user IDs
must never be accepted as bearer credentials.

The node authenticates the device credential, then applies authorization from the
committed grant: mode, source addresses/prefixes, destination routes, private-network
rights, region and subscription expiry. No decision binds to public source IP.
For proxy mode, private destinations are independently denied unless granted; for
VPN mode, source spoofing is denied by the packet policy and private destination
access must also be enforced by the host firewall/router.

Use a bounded node cache so a tunnel does not query PostgreSQL per packet/flow.
Recommended initial policy: synchronize grants every 30 seconds, expire each node
lease at the earlier of subscription expiry or five minutes without a fresh
control-plane confirmation. Revocations should push immediately and also appear in
polling snapshots. Apply unchanged snapshots without replacing existing leases;
actual policy/credential changes close old sessions. `DeviceRegistry::renew()`
refreshes an active lease deadline without cancelling its flows; expired or
revoked leases cannot be revived. Fail closed after the lease
horizon if synchronization fails. The exact horizon is an availability/revocation
tradeoff, not a protocol constant.

Logout-all, installation deletion, disabled account and subscription withdrawal
revoke grants. A roaming client reuses its device credential and address, creates
a fresh protected carrier and a new packet session; old TCP flows cannot be restored.
IP/prefix ownership and device slots must be released only after awaited teardown.

JWT access tokens, refresh-token machinery and mTLS would add key rotation, replay
and revocation complexity here. Start with opaque device credentials and bounded
node grants. If proof of device-key possession is later needed, add it as a separate
credential mechanism through `Authorizer`, keeping the client/server protocol core
independent of the account database. CONNECT-IP transport remains based on
[RFC 9484](https://www.rfc-editor.org/rfc/rfc9484.html).

## Validation

The library/backend integration tests cover imported config persistence and
redaction, capsule confirmation, bidirectional IP packet IO, awaited endpoint
release, wrong token, credential rotation, reauthorization on an existing carrier,
mode withdrawal, revocation, renewal and expiry of already-open streams. A successor
carrier takes over the device VPN session only after the old packet ownership is
released. The suite passes with the default Rustls profile and with the Chrome
profile selected through the imported configuration. Existing actor and
provision tests remain enabled. Android arm64 compilation is distinct from Android
runtime acceptance.

Run `uv run --no-project tests/integration/test_foldfin.py` for real Linux actor/TUN acceptance
in disposable namespaces. It builds both checkouts, checks MTU, routed
TCP/FIN/UDP/DNS/ICMP, and verifies address/route rollback on Ctrl+C. It skips DNS
configuration and uses an isolated config directory; host routes/DNS are untouched.
