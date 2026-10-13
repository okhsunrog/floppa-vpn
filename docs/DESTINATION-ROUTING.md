# Linux destination routing

The Linux client can select an outbound path for individual IPv4 addresses and
networks. These rules belong to the local machine; they do not change server-side
private networks or the Android application split-tunnel list.

In **Settings → Destination routing**, add a name (optional), an address or CIDR,
and one of:

- **Direct via physical network**: use the lowest-metric physical default uplink
  (Ethernet or Wi-Fi), even when another VPN has more specific routes in `main`.
- **Through Floppa VPN**: use the Floppa interface, even when another VPN has routes
  covering that destination. The VPN peer must permit the destination.
- **Use system routes**: use Linux's `main` table, including any routes installed
  by other VPNs. This is useful as an exception to a broader rule. It does not
  necessarily mean direct Internet access.

Changes are saved locally. Use **Reconnect to apply** to apply them immediately,
or connect normally later. The system service persists accepted connection
parameters for subsequent reconnects. GUI settings and CLI rule files are
separate: connecting from the GUI applies its saved list again.

A host is treated as `/32`; CIDRs are normalized to network addresses. The more
specific prefix wins, regardless of input order. Duplicate networks are rejected.
Up to 64 rules are supported. A `0.0.0.0/0` rule can choose the default path, with
more specific exceptions. Local kernel routes (priority 0) and the Floppa
transport endpoint remain exempt.

## CLI

Create a JSON array, for example `routes.json`:

```json
[
  {"name": "Corporate VPN gateway", "destination": "194.186.63.0/24", "target": "direct"},
  {"name": "Personal VPN destination", "destination": "160.79.104.0/22", "target": "tunnel"}
]
```

```sh
floppa connect --routes ./routes.json
```

Without a stored API login, the Linux service reuses its stored tunnel configuration.
With a login, the CLI provisions the selected protocol as usual. Both paths apply
the supplied destination rules. An empty array clears the rules. Without an API
login, omitting `--routes` resumes the stored connection and
preserves its rules. For API-based or foreground connections, pass the rule file
on each invocation; otherwise that new connection has no destination rules.
The option also works with `connect --config <file>`.

## Scope and coexistence

This version supports Linux IPv4 only. Domains, IPv6 destination rules and
per-application routing are not implemented. DNS configuration remains separate;
a destination rule does not pin a domain or control its AAAA records. Direct
routing currently requires a physical interface with a default IPv4 route;
bridge, bond and PPP uplinks are not automatically selected. The uplink is chosen
at connection time; reconnect after changing uplinks if the tunnel has stayed up.

The policy uses priority 89 for the transport endpoint, priorities 90–153 for
user rules (most specific first), and dedicated tables 52600–52663. Higher-priority
rules installed by other software still take precedence. Occupied policy slots
or tables cause an explicit connection error rather than being overwritten.
Existing Floppa routes, LAN exclusions and DNS remain the fallback for destinations
without a matching rule. Explicit user rules take precedence over LAN exclusions.

Rules and routes are tagged with protocol 186. Every applied policy is recorded
in the durable rollback journal before mutation. Disconnect, failed startup and
service recovery remove the exact recorded objects, preserving other VPN routes.
Each dedicated table has an unreachable fallback, so disappearance of its selected
interface cannot silently redirect matching traffic through another VPN while the
policy is active. This is not a persistent kill switch: normal disconnection removes
these rules and restores ordinary routing.

The GUI/CLI, tunnel service and privileged helper must be updated together. RPC
protocol 5 prevents a new client from silently handing these settings to an older
service that would ignore them.

## Testing without changing host routing

```sh
sudo unshare --net bash tests/routing/policy.sh
```

The test refuses the host network namespace. It checks direct and tunnel routes
alongside a simulated corporate VPN, overlapping system exceptions, endpoint
exemption, occupied slots, interface disappearance and idempotent exact cleanup.
It uses dummy interfaces, so it validates kernel route selection, not remote
server reachability or a physical VPN session.
