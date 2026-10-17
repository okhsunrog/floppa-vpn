# Foldfin server operations

The server path is `floppa-server` (authenticated control API), PostgreSQL (existing
accounts, subscriptions and installations), and `floppa-foldfin --features node`
(regional packet process). The protocol crate owns neither accounts nor deployment.
The node shares one persistent TUN/router between its TCP/H2 and UDP/H3 listeners.
It serves device-level VPN; the standalone Foldfin CLI retains its SOCKS proxy mode.

## Provisioning API

1. Authenticate through the existing Floppa API and bind the session to an installation
   with the existing installation endpoint. Select an entitled region.
2. `POST /me/foldfin/config` with `{}` creates a device grant. A second request returns
   409; use `{"rotate":true}` explicitly to replace a lost or compromised credential.
3. The 201 response contains `device_id`, `revision`, and a Foldfin v2 `config` document.
   Wrap it as `{"protocol":"foldfin","config":...}` for the existing client import.
   The response has `Cache-Control: no-store`. Store the secret privately on the client.
4. `GET /me/foldfin/devices` lists owned allocations and node acknowledgement, without
   token plaintext or digest. `DELETE /me/foldfin/devices/{id}` revokes an owned grant.

Tokens contain 256 random bits, are returned once, and only SHA-256 digests are stored.
A device uses one quota slot across WG/AWG/Foldfin. Subscription locks serialize
concurrent issuance; regional allocation locks prevent address collisions. Addresses
stay stable through token rotation and reconnection. Logout revokes grants issued by
that API session; changing installation region revokes its previous grants.
Subscription expiry, region entitlement withdrawal, installation deletion and node
disable are reconciled by the node. A credential is a bearer secret, not a hardware
identity. The response is provisioning, not proof that the node is currently serving.
Private-network, speed and subscription-plan changes increment the configuration
revision. A client with an older revision should explicitly reprovision/rotate;
simple expiry extensions preserve the revision and live connection.

## Reconciliation and policy

The node subscribes before its initial repeatable-read snapshot. Notifications are
hints; a periodic complete snapshot remains authoritative. Every row is validated
before applying any update. An unchanged grant extends its lease and preserves open
flows. Changed token, speed or private-network rights replace the grant and close its
old sessions. A successful apply acknowledges only the matching device revision.

Default sync interval: 30 seconds. Default offline authorization horizon: one hour,
always shortened to subscription expiry. A failed read/validation does not extend old
leases. Expired leases cannot be revived; reconciliation can install a new lease for
a fresh connection. Set `lease_secs` to the desired outage/revocation tradeoff
(2 sync intervals to 24 hours). `last_sync_at` and `applied_revision` are observational;
a stale heartbeat must not be treated as current readiness.

The bounded node supports 4096 active grants. Source addresses are checked by Foldfin;
Floppa's packet adapter denies client-to-client and ungranted private destinations in
both directions. A default route does not grant private access. Explicit plan private
networks supplement the node routes. Pools cannot overlap another Foldfin region.
Kernel ICMP errors quoting the client's packets remain usable for PMTU/TTL discovery.
Each device has a separate token bucket in each direction, using the plan Mbps limit
with a 64 KiB burst. This is per-device bandwidth, not aggregate account accounting.
There is no traffic-quota accounting feature added by this change.

## Deployment

Build `cargo build --release -p floppa-foldfin --features node`, or
`just package-foldfin-server` for the static musl artifact. The latter includes config,
service, SQL role and certificate hook examples and both source revisions. The musl
builder fetches Foldfin and its H3 patches from the public Git repository at the
locked revision; no adjacent source checkout is required. The packaged
`FOLDFIN_REVISION` records this dependency revision.

Apply the main Floppa migrations, including `0025_foldfin.sql` and `0026_foldfin_policy_revisions.sql`, through the normal API
deployment. Create the `floppa-foldfin` system user. Install the binary in
`/usr/local/bin`, config in `/etc/floppa-foldfin/config.toml` and secrets as
`root:floppa-foldfin`, mode 0640. Start from `floppa-foldfin/deploy/` examples.
Use a public CA certificate for the configured DNS name; do not distribute a private
CA key. IPv4-only nodes omit `ipv6_pool`, IPv6 routes and IPv6 DNS together.

Run `floppa-foldfin --config <file> network --uplink <physical-egress>` to render
networkd TUN/address sections and a dedicated nftables table from the public plan.
Review and split the printed sections into the named networkd files and an nftables
include. Load them using the host's existing network/firewall manager. The node never
runs `ip`, changes sysctls or rewrites the default route. Explicitly enable forwarding
in deployment, permit the listen port over TCP **and** UDP, and permit forwarding
between TUN and egress in the existing firewall. The example uses IPv4/IPv6 NAT for
internet exits; routed deployments replace NAT with upstream return routes. Private
networks on another interface need an explicit firewall adaptation. Check pools
against existing WG/AWG, LAN and other host routes before registration.

With deployment credentials, run `floppa-foldfin --secrets <deployment-secrets> register`
once. Registration is explicit and never happens during normal restarts. Metadata is
immutable while allocations exist; drain/reprovision explicitly when changing pools,
endpoints, routes, DNS or trust anchors. Keep the deployment credentials out of the
service. `deploy/node-role.sql` documents a restricted serving role (read grants,
update acknowledgements/heartbeat only); provision its password separately and require
verified PostgreSQL TLS. The role observes all regions; per-region RLS is not present.

Install `systemd/floppa-foldfin.service`, start it after the persistent TUN exists,
and observe the journal and loopback-only metrics on port 9104. The service owns no
CAP_NET_ADMIN, firewall or routing privileges. It exits on packet-device or listener
failure so systemd can restart it. SIGTERM/SIGINT joins listeners and the router.
Stopping it leaves the deployment-owned persistent interface and firewall in place.
Remove those through the same network/firewall manager when uninstalling.

## Certificates

Use the host's existing ACME client for issuance/renewal. Install
`scripts/foldfin-install-certificate.sh` and configure the ACME deploy hook as:

```
/usr/local/sbin/foldfin-install-certificate vpn.example.com "$RENEWED_LINEAGE"
```

The hook checks SAN, near-term expiry and key correspondence, installs root-owned
0640 PEMs into a versioned directory, atomically switches `tls/current`, and requests
systemd reload when running. Run it once before initial registration/start. SIGHUP
validates the whole PEM pair and atomically updates the shared H2/H3 resolver. Invalid
reloads keep the old identity; existing TLS connections survive valid rotation.
Monitor reload failures and certificate expiry. Previous identity directories are
retained for explicit rollback; prune them according to deployment retention policy.
Self-signed development certificates require explicit client trust and a separate
trust-anchor migration when replaced.

## Validation boundary

Database/API tests exercise policy revisions, quota races, ownership, digest storage, stable rotation,
logout, region change and metadata conflicts. Real local H2/H3 tests exercise routed
IPv4/IPv6 packets, policy isolation, unchanged lease renewal, rotation, failed snapshot
expiry, restoration, subscription deadlines, actual DB-pool unavailability and explicit
revocation. Packet-device failure closes both listeners. TLS tests verify both carriers with live
connections across certificate replacement. The three-namespace `tests/integration/test_foldfin_node.py` also passed with the
static musl node running unprivileged, real TUN and generated nftables rules: IPv4/IPv6
TCP, UDP and ICMP over H2/H3, followed by joined process and namespace cleanup.
These local checks do not imply a Singapore deployment or physical Android acceptance.

To repeat that acceptance, use a fresh migrated disposable database and pass its Unix
socket URL with `--database-url`, built node/client paths and a new artifact directory
with `--output`. It creates no host veth, changes no host routes and makes no WAN requests.
Only namespace forwarding sysctls are changed. Fixture DB rows are intentionally left
in the disposable DB for inspection; drop that DB afterward.

Server rollout still needs a prepared deployment TUN/firewall, ACME integration and
physical-interface Singapore acceptance in the isolated namespace harness. Existing
proxy/static-peer tests are not evidence of this new multi-user node running remotely.

Validation on 2026-10-17: 177 Rust tests passed across core/server/node suites, one
existing external Telegram TLS test remained ignored. Workspace offline check,
Clippy with warnings denied for project code, Rust formatting, regenerated OpenAPI
and SQLx cache, frontend build, shellcheck, nftables check and systemd unit validation
passed. The node built as static musl ELF. Existing vendored h3 warnings are unchanged.
Artifacts: `/home/okhsunrog/tmp_zfs/codex_tmp/foldfin-server-review/`;
real namespace acceptance: `node-netns-1/`.
