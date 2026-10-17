# Foldfin integration and access design

The client now imports Foldfin JSON into the existing config store, selects it as
`Protocol::Foldfin`, and runs the shared engine through `TunnelManager` and the
connection actor. Routes, DNS, rollback, reconnection and platform consent remain
owned by Floppa. No extra VPN process or reconnect loop is introduced.

Linux opens the persistent TUN prepared by the existing helper, applies MTU before
routes, and marks outer TCP/UDP before connect using the mandatory shared Linux SO_MARK policy. Android prepares TCP/UDP
before connect through a service-generation-owned Underlay, protects and binds
each socket to the underlying Network, aborts on preparation failure, and gives Foldfin an owned CLOEXEC
duplicate of the nonblocking Kotlin descriptor. SNI and HTTP authority are kept
separate from the resolved dial address. Foldfin is not implemented on Windows or
macOS yet. Host Rustls and Android builds use certificate/name verification;
TLS uses rustls. The former Chrome/BoringSSL backend has been removed.

## Import and run

Development dependency: `floppa-vpn-core` and `floppa-foldfin` currently use the
local checkout at `../../rust/foldfin`. Keep this worktree in `~/code/floppa-vpn-foldfin` and
Foldfin in `~/code/rust/foldfin`. Release packaging/CI needs a published or pinned
Git Foldfin dependency before distribution; the current local path is intentional.

Import this document through the existing app import action or
`floppa import <file>`. For a temporary configuration, use
`floppa connect --config <file> --no-dns`. The normal service/standalone ownership
selection applies; `--config` changes input, not ownership. Store the document privately:
it contains a device bearer credential. The token below is a dummy example.

```json
{
  "protocol": "foldfin",
  "config": {
    "version": 2,
    "endpoints": ["vpn.example.org:8445"],
    "transport": "h2_only",
    "server_name": "vpn.example.org",
    "token": "0123456789abcdef0123456789abcdef",
    "network": {
      "revision": 1,
      "addresses": ["198.18.77.2/32", "fd42:77::2/128"],
      "peers": ["198.18.77.1", "fd42:77::1"],
      "routes": ["198.18.77.1/32", "198.19.77.0/24", "fd42:77::/64"],
      "excluded_routes": [],
      "dns": [],
      "mtu": 1280
    }
  }
}
```

Optional `additional_ca_pem` embeds a development CA; production normally omits
it and uses public trust anchors. Version 1 is explicitly rejected; reissue it as v2.
The cached network plan carries both address families, canonical routes, explicit
exclusions, DNS and an MTU of 1280. Android derives routes from this plan instead
of unconditionally capturing all traffic. Desktop setup records each address
separately in rollback. Android owns "block connections without VPN" behavior.
`auto` gives H3 a two-second candidate budget, then tries H2 on network or
capability failure within a shared 15-second deadline. Certificate, authorization
and malformed-protocol failures never trigger transport fallback. `h2_only` and
`h3_only` force their respective transport. Both use the same persistent packet
session; network changes replace its carrier while retaining the device.
Initial connection walks the configured endpoints and their resolved address
candidates through Floppa's shared resolver and rollback ladder. Carrier-only
recovery refreshes DNS through its session underlay and walks configured endpoints
and address candidates without rebuilding the device. Socket preparation is session-scoped:
`TunnelManager::start_foldfin_packets_on` takes an `Arc<dyn Underlay>` retained
across recovery. Linux's native policy applies SO_MARK; Android captures the exact
Java VpnService instance when its TUN descriptor arrives. Retired generations
cannot prepare new sockets, and queued network-change events naming an old
Foldfin generation are ignored under the tunnel replacement lock. No global
socket callback is used by Foldfin. Other protocol adapters retain their existing
socket integration. Android system lockdown remains owned by Android.
H3 requires path capacity for a 1280-byte IP packet plus HTTP/QUIC overhead.
The workspace and Tauri workspace patch h3/h3-datagram from Foldfin's vendor
folder; those patches are required for CONNECT-IP and nonzero request IDs.

Arbitrary host/LAN routes in the GUI do
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
released. The suite uses rustls with H2 and H3; Chrome profile support was removed. Existing actor and
provision tests remain enabled. Android arm64 compilation is distinct from Android
runtime acceptance.

Run `uv run --no-project tests/integration/test_foldfin.py` for real Linux actor/TUN acceptance
in disposable namespaces. It builds both checkouts, checks MTU, routed
TCP/FIN/UDP/DNS/ICMP, and verifies address/route rollback on Ctrl+C. It skips DNS
configuration and uses an isolated config directory; host routes/DNS are untouched.

## Underlay validation (2026-10-15)

238 core unit tests and eight integration tests pass, including preparation
refusal without TCP/UDP transmission, retained preparation across recovery and
stale service notifications. Host and Android core/application checks and Clippy
pass. An arm64 debug APK builds, including Kotlin compilation. Debug Gradle
configuration no longer requires release signing credentials.

The additional Android lint run reports one pre-existing GradleDependency finding:
core-ktx 1.19.0 is pinned while 1.19.1 is available. No source finding was reported.
Physical service/network-roaming acceptance is pending; the attached Pixel's
active VPN has not been interrupted. Underlay DNS and actor recovery remain open.

The installed Pixel application has a different signing certificate from the debug
build. No replacement/uninstall was attempted. A side-by-side debug package was
assembled with a temporary Gradle init script setting `.foldfinlab` as its
application ID suffix; native code is the same validated arm64 build.
Artifact: `/home/okhsunrog/tmp_zfs/codex_tmp/foldfin-underlay/artifacts/floppa-foldfin-lab-arm64-debug.apk`.
The artifact's package is `dev.okhsunrog.floppa_vpn.foldfinlab`; installation and
switching the active VPN are deferred until implementation and Floppa integration
are ready, per the user's revised validation order.

## Integration with main (2026-10-15)

Rebased onto main `60bc70b`. Main now owns persistent retry policy, revisioned
settings, endpoint candidate discovery/cache, shared Linux recovery and mandatory
socket marking. Foldfin must extend these mechanisms rather than create parallel
settings, DNS caches or recovery loops.

- Desktop Foldfin uses the same fixed socket mark as WG/VLESS. Marking failures
  remain typed permission failures, so the actor does not treat them as network
  verification failures. Packet-channel tests supply an explicit test Underlay.
- Shared endpoint cache identity includes Foldfin's TLS name and configured CA,
  never the bearer token. Credential rotation does not discard endpoint knowledge.
- Provisioned route exclusions use the existing shared subtraction function;
  Linux user exclusions are applied afterwards by the desktop ladder. Every dual-stack
  address is journaled before mutation; a journal write failure aborts setup.
- Config import follows main's service/standalone ownership rules. Existing saved
  protocol allowlists remain authoritative; adding Foldfin does not silently
  append it to a user's saved automatic selection order.

The actor now recovers a lost Foldfin carrier while retaining its packet device
and platform rollback stack. Retry limits, backoff, offline parking, cancellation,
and settings/intent changes remain actor-owned. Cancellation and timeout join the
carrier task before teardown; terminal failures publish their outcome after
rollback. Observations started before successful recovery cannot trigger a second
recovery. The existing public Retrying phase represents this internal Recovering
state and retains tunnel metadata and the applied settings revision.

Android service-generation notifications invalidate the bound Foldfin carrier;
the actor starts recovery. Stale service notifications are ignored. Main's Windows
watcher still emits NetworkChanged, whose actor handler unwinds the full tunnel.
Linux observes availability and invalidates resolver discovery on rtnetlink events;
it does not emit that teardown command on route changes. Marked transport sockets
follow OS routing without endpoint host-route rewrites.

Carrier health and received payload are distinct: a successful carrier probe
does not update the last received payload timestamp. Probe evidence still informs
silence detection; it does not establish working DNS or forwarding.

Initial attempts now walk every provisioned endpoint through main's candidate
resolver. A selected endpoint supplies the HTTP authority and port while retaining
the provisioned TLS name/CA. Selection is attempt-local and never rewrites stored
configuration. Existing attempt deadlines and cancellation bound the whole walk.

Foldfin maps classified network/capability failures to EndpointUnavailable, allowing
the next address only after successful rollback. ConnectionRejected covers TLS,
authorization, policy and protocol rejection: it stops both endpoint fallback and
the actor's protocol/retry cycle. Socket permission failures remain fatal as well.
Android preserves these typed backend errors instead of flattening them into a
service-error string. Unclassified local engine errors do not permit address
fallback. UI bindings and both locales expose the new failure categories.

Carrier-only recovery now uses the shared resolver cache with caller-owned DoH
discovery over session-prepared sockets. Its four-second DNS budget queries A/AAAA
through Google/Cloudflare HTTPS endpoints with IPv4/IPv6 bootstrap addresses.
Recovery does not use system DNS or the process-global Android network hook;
Linux SO_MARK and Android's captured service preparation also apply to these DNS
connections. Private names require existing cached candidates or an explicit host
Underlay resolver implementation. Initial connection discovery is unchanged.

Fresh answers precede cached candidates; DNS never establishes server identity.
A network-epoch change rejects the discovery result. Permission refusal cannot
fall back to cached candidates. DNS failure advances to the next provisioned
endpoint; transport failure advances after releasing the failed carrier.
TLS/auth/policy rejection stops the walk. Actor cancellation/deadlines bound the
whole operation. Successful selection updates the reported authority while leaving
the provisioned endpoint order, TLS name/CA and network plan unchanged.

Device/WAN acceptance is deferred. Local compilation and regressions validate the
rebase; they do not establish Android roaming or production deployment readiness.

Rebase validation: core/adapter/CLI check and Clippy with warnings denied pass;
340 Rust tests pass (one privileged namespace test remains ignored). All 91 client
JavaScript tests and `vp check` pass after syncing main's dependencies. Desktop Tauri and JNI/Android
arm64 all-target compilation pass. Logs are under
`/home/okhsunrog/tmp_zfs/codex_tmp/foldfin-rebase/`. Vendored h3 emits existing
compiler warnings; no vendor changes were made in this rebase.

Candidate regression coverage includes rollback before a second address, stopping
on certificate/authorization/permission failure, selection across endpoint ports
without changing TLS identity or stored order, actual H2 connection refusal, and
H2/H3 authorization rejection. These are local checks without host route changes.

Candidate increment validation: 278 core unit tests, 12 Foldfin integration tests,
91 client JavaScript tests, host all-target checks/Clippy, Android application
all-target Clippy, and frontend format/lint/type checks pass. TypeScript bindings
were regenerated with the application's export example. Logs:
`/home/okhsunrog/tmp_zfs/codex_tmp/foldfin-candidates/`.

Retained-carrier increment validation: 284 core unit tests and 12 local Foldfin
integration tests pass, including H2/H3 device retention, notification invalidation,
probe/payload separation, stale observations, retry exhaustion, cancellation,
timeouts, reconfiguration and offline parking. Host core/adapter/CLI all-target
check and Clippy, Android application all-target Clippy, and Rust formatting pass.
Logs: `/home/okhsunrog/tmp_zfs/codex_tmp/foldfin-recovery/`. No device or WAN
acceptance was run for this increment.


Scoped discovery validation: 14 resolver/relay unit tests, 284 core unit tests and
13 Foldfin integration tests pass. Coverage includes preparation refusal before
transmission, literal IPv6 without DNS, cached candidates not bypassing permission
refusal, TLS verification through the relay, relay closure on cancellation, and
recovery across multiple endpoints/addresses with the original device and TLS
identity. Host all-target check/Clippy and Android all-target Clippy pass.
Logs: `/home/okhsunrog/tmp_zfs/codex_tmp/foldfin-discovery/`.
Public DoH availability and physical roaming were not tested.

Physical underlay validation (2026-10-17): fresh scoped DoH passed from a disposable
namespace through Ethernet enp0s13f0u1u5, while an explicit blackhole blocked
unmarked external sockets. The marked SSH marker to Singapore was matched by
source port against server ss and captures on both the physical NIC and VPS.
The host's active VPN was retained; administration SSH used its existing route.
This validates Linux scoped discovery and marked outer connectivity, not a remote
Foldfin carrier, full VPN recovery, Android roaming or IPv6 WAN forwarding.

The harness is tests/integration/test_foldfin_underlay.py, with the
floppa-vpn-core/examples/foldfin_underlay.rs diagnostic. Build that example and
pass its absolute path with --binary and a new disk-backed --output directory.
It never reads provisioning credentials or changes the deployed Foldfin service.
Example build/artifacts should use
/home/okhsunrog/tmp_zfs/codex_tmp/foldfin-physical-discovery/ via
CARGO_TARGET_DIR/TMPDIR.

Initial failures exposed two fixture requirements: firewalld's nftables forward
hook rejects the new veth despite iptables ACCEPT, and the synthetic blackhole
breaks reverse-path checking in the namespace. The helper temporarily assigns
only its disposable veth to firewalld's trusted zone and disables rp_filter only
on namespace outer0. Cleanup restores zone membership, deletes exact tagged
rules/routes and namespace, stops its transient remote capture unit, and checks
host defaults, rules, forwarding, TCP buffers, rp_filter and resolv.conf.

Final evidence: /home/okhsunrog/tmp_zfs/codex_tmp/foldfin-physical-discovery/run-5/.
Both passed and cleanup are true. Runs 1–3 preserve the diagnostic failures;
run 4 first established success. No fallback through the host VPN was used.
Post-change validation: 311 Rust tests and host all-target check/Clippy pass.

Remote carrier recovery passed on 2026-10-17 through the physical Ethernet path
to Singapore. Extend the underlay harness with --recovery-client (the built
foldfin_recovery example) and --server-binary (current static Linux Foldfin CLI,
built with h3). The helper deploys a disposable server on port 18445 with an
ephemeral token, separate persistent TUN and 198.18.79.1/198.18.79.2 static peers.
It restricts the outer listener to the source established by the physical marker.
It does not upgrade or restart the managed foldfin service.

The real Floppa actor runs in the client network namespace and a private mount
namespace. /run/floppa-vpn and /var/lib/floppa-vpn are bound to disposable
directories so ownership and rollback cannot touch the host's active tunnel.
H2 fault injection destroys only the namespace's TCP socket to the lab listener;
H3 fault injection restarts only the lab server. Both pass Connected -> Retrying
-> Connected with unchanged ifindex, addresses, MTU, route/rule snapshots and
connected_at. Before and after each fault, four concurrent TCP/FIN transfers,
UDP, direct DNS queries and ICMP pass. Explicit Down completes rollback.

Evidence: /home/okhsunrog/tmp_zfs/codex_tmp/foldfin-recovery-wan/run-2/.
Local and Singapore captures include the actual carriers, not only the marker.
Both transport results, local cleanup and remote cleanup pass; the production
server remains active with the same MainPID. The earlier run stopped on a binary
upload timeout and completed cleanup before any tunnel test. Uploads now have a
separate 120-second administration timeout.

This acceptance uses IPv4 routed VPN traffic, not internet full tunnel. OS DNS
application is disabled (manage_dns=false); DNS packet forwarding and preservation
of the host resolver are verified, not resolved/resolv.conf reconfiguration.
It does not claim preservation of pre-existing inner flows, prolonged outage
recovery, physical IPv6 routing or Android roaming. The example builds and passes
Clippy; the runtime acceptance above exercises the new harness.
