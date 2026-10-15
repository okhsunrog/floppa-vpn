# Excluding destinations from Floppa VPN

On Linux, **Settings → Exclude from Floppa VPN** accepts IPv4 addresses and
networks. For example, exclude `194.186.63.0/24` to let the corporate VPN's server
connection use ordinary system routing instead of Floppa.

Floppa leaves these addresses out of its host routes. It does not select an
interface, pin a gateway, allocate policy tables or install `ip rule` entries.
NetworkManager continues to manage the physical default route, including changes
between Ethernet, Wi-Fi and USB tethering. Other VPN routes and policy rules are
still respected: an exclusion bypasses Floppa, not necessarily every other VPN.

## CLI

Save a local `routing.json`:

```json
{
  "excluded_routes": ["194.186.63.0/24"]
}
```

```sh
floppa connect --routes routing.json
```

An explicit empty list clears exclusions. With the Linux system service, omitting
`--routes` reuses active or last-successful settings, including after disconnect
or service restart. Both imported-config and API-login paths preserve them.
The service saves settings after a successful connection. GUI settings are saved
separately; connecting from the GUI applies its own list.

For a foreground connection with `--config`, supply the file on each invocation;
without it that new foreground connection has no exclusions.

Hosts become `/32`, CIDRs are normalized, and duplicates are removed. Invalid
input is rejected before replacing the connection intent. Up to 64 entries are
accepted. Domains and IPv6 exclusions are not currently supported. DNS is a
separate setting; excluding a subnet does not configure split DNS.

## Implementation

`floppa-tunnel-config::route::exclude_networks` subtracts exclusions from the
positive route set after LAN handling and server-granted private routes. Explicit
exclusions therefore win over overlapping grants. WireGuard peer `AllowedIPs`
remain unchanged. The existing `Step::Routes` journal records the resulting list
before installation and removes it on rollback/disconnect.

Subtraction can produce narrower routes around an excluded subnet. Normal Linux
longest-prefix selection still applies when another VPN has overlapping routes;
this feature does not promise priority over every possible corporate routing
configuration. The excluded subnet itself is never covered by generated Floppa
routes. Floppa's existing transport-endpoint host route handling is unchanged.

Client and service must be upgraded together (RPC version 6). The withdrawn
three-target routing config is deliberately rejected by `--routes` rather than
silently interpreted as an empty exclusion list.

## Verification without touching the active tunnel

```sh
cargo test -p floppa-tunnel-config --test exclusions
cargo test -p floppa-tunnel-config --test exclusions linux_network_handover -- --ignored
```

The second command requires passwordless sudo and creates an isolated network
namespace. It passes routes calculated by the production Rust function to Linux,
checks exclusion and corporate-route selection, changes physical defaults without
rewriting Floppa routes, and checks cleanup with the production helper. Dummy
interfaces validate route selection, not real remote VPN connectivity.

Changes saved through the owner API now reconcile an active configured tunnel automatically. Temporary profiles retain their explicit parameters. Saved and applied revisions are reported separately in TunnelState.
