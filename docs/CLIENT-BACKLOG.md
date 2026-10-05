# Client backlog

The shared client engine already exists: `floppa-vpn-core` owns the actor, tunnel
engines, config store, rollback and RPC; `floppa-api-client` and `floppa-provision`
own the shared API/provisioning and peer repair logic. Linux CLI, desktop GUI,
the system tunnel service and Android use these crates. Android hosts the actor
in `:vpn`; Kotlin/JNI remains responsible for platform VPN consent, TUN creation,
socket protection, network callbacks and foreground notifications.

This backlog records follow-up work, not a claim that the current implementation
has passed desktop or Android runtime acceptance.

## Local routing policy and coexistence with OpenVPN

- [ ] Add typed local destination rules: direct via the physical network, through
  Floppa, or according to system routes. Provide a local config and GUI editing
  over the same model; do not require arbitrary privileged shell hooks.
- [ ] On Linux, use a dedicated Floppa routing table and explicit policy rule
  priorities so OpenVPN routes in `main` can coexist with Floppa. Define precedence
  for endpoint bypass, explicit user rules, granted private networks and defaults.
- [ ] Support IP/CIDR rules first. A direct rule must select the physical uplink,
  not merely `main`, which can also contain VPN routes. Track uplink changes and
  remove only routes/rules owned by Floppa.
- [ ] Add hostname rules with A/AAAA handling, refresh and a defined resolution
  failure policy. Keep the corporate VPN transport outside Floppa while allowing
  corporate destination routes to remain owned by OpenVPN.
- [ ] Define DNS coexistence and behavior when OpenVPN redirects all traffic;
  test both split and full corporate tunnels, reconnect, uplink changes and both
  disconnect orders in an isolated environment without changing the host VPNs.
- [ ] Carry the policy through the shared core and RPC, with explicit platform
  support. Do not promise Linux policy-routing behavior on Android, where VPN
  ownership and routing are controlled by the OS.

## Shared client behavior and service integration

- [ ] Align and document CLI/GUI service selection and errors. In particular,
  decide consistent behavior for a missing service, denied socket access and an
  incompatible RPC version; avoid silently starting a competing Floppa actor.
- [ ] Consolidate remaining shared orchestration where behavior should match:
  connection parameters, session handoff and RPC client initialization. Keep
  presentation, process hosting and OS adapters in their respective applications.
- [x] Share system-service RPC client construction between Linux CLI and GUI.
- [ ] Audit preservation of private routes and future local policy across stored
  config connection, resume and boot; `connect_stored` currently constructs fresh
  parameters only when no successful request has been recorded yet.
- [x] Preserve the recorded parameters when CLI connects using the service's
  stored configuration; update the boot record when parameters change even if
  the protocol order stays the same, serialize writes and retry failed saves.
- [ ] Prevent competing Floppa owners even when they select different TUN names.
  This must not prohibit an independent OpenVPN tunnel.
- [ ] Harden the systemd unit after validating required TUN, routing, state-store
  and DNS operations, including the `/etc/resolv.conf` path.
- [ ] Remove stale service comments and documentation that describe already
  implemented client control or boot restoration as unfinished.
- [ ] Verify packaged socket activation, group permissions, CLI/GUI control of
  the same tunnel, GUI exit/reopen, session refresh/sign-out, boot restoration,
  version mismatch and crash cleanup. The current host has no installed
  `floppa-vpn.service` or `floppa-vpn.socket` as of 2026-10-05.
- [ ] Verify the shared behavior on Android as well: UI process loss, service
  lifecycle, network changes, offline recovery, peer repair and log export.
- [ ] Track a native Windows service/IPC implementation separately; current
  desktop system-service integration is Linux-specific.

## Diagnostics

Android diagnostic capture is primarily a convenient way to collect app and VPN
process logs without requiring adb. GUI capture can also be useful on desktop.
The Linux system service already logs to journald; missing journald integration
in GUI export is not incomplete service logging and is not a required fix.

- [ ] Document collection of GUI diagnostics and service logs together when
  needed. A basic service export is
  `journalctl -u floppa-vpn.service --since "30 minutes ago" --no-pager > floppa-vpn-service.log`.

A combined GUI/service support archive is an optional future convenience. It is
not currently scheduled; reconsider if support cases show that manual collection
is a recurring obstacle.
