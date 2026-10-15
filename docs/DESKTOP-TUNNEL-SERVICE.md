# The desktop tunnel service

The Linux counterpart of the Android `:vpn` split, and for a related reason. On Android the UI
process is frozen in the background, so an actor living there cannot reconnect a tunnel that died
while the phone was in a pocket. On a desktop the UI process is not frozen — it is *closed*, and
the tunnel goes with it.

`floppa service` is the process that holds it instead. The same binary as the command-line client,
started a different way.

## Three ways a tunnel gets onto a machine

Not two. The service is the one to prefer where it exists; the others are what every other
platform has, and what a native package or a local build has on Linux.

| mode | holds the actor | how it gets privileges | store | when |
|---|---|---|---|---|
| **service** | `floppa service`, as root | it *is* root | `/var/lib/floppa-vpn`, `0700` | Linux with the package |
| **in-process, desktop user** | the app, or `floppa` as a user | Linux: `CAP_NET_ADMIN` for socket marks, existing `pkexec` helper for network changes | the OS keyring | native Linux packages, Windows, macOS, socket disabled |
| **in-process, root** | `sudo floppa --config …` | already root | nothing persisted | one-shot runs, the test containers |

The second row runs the actor inside the client. It is also the only mode on
Windows and macOS, so it cannot rot: everyone who is not a packaged Linux user runs it.

## Choosing one

A client decides once, at startup, by **connecting** to `/run/floppa-vpn/vpn.sock` —
`client_mode::probe`. Connecting, not checking whether the file is there: a process that dies
leaves its socket behind, and that file will sit there refusing connections indefinitely.

The answer is kept for the whole run. Two actors must never both be live, because the second takes
the interface the first is using and lays its own routes and DNS over them while holding a
rollback journal that knows nothing about what the other applied. The privileged helper refuses
the overlap as a floor under this: `ensure-tun` will not hand over a TUN that belongs to another
uid.

Five answers:

- **Available** — drive the service.
- **Absent** — nothing is listening. GUI may run the tunnel in its own process.
  CLI `connect` without a config requires the service; `connect --config` explicitly
  selects an independent foreground run.
- **Forbidden** — a service *is* there and this user may not talk to it. This is not absence, and
  treating it as absence could start a competing local actor. Neither CLI nor GUI
  falls back in this case; the user is told how to obtain access to the `floppa` group.
- **WrongVersion** — a service is there speaking a protocol this build does not. Almost always an
  upgrade that replaced the binaries while the old service kept running. Restart it.
- **Unresponsive** — the socket accepts connections but the service cannot answer,
  or an unexpected socket error prevents determining its owner. This is not absence
  and does not authorize a local actor.

GUI setup checks socket reachability without waiting for a service cold start.
The settings page checks health asynchronously and can refresh it when reopened.
Every newly opened RPC connection checks the protocol version before any mutation,
including on Android; an incompatible service receives no control commands.

## Installing and enabling

The package ships the binary and five files, and puts nobody in the group.

```bash
sudo usermod -aG floppa "$USER"      # then log in again, so the new group applies
sudo systemctl enable --now floppa-vpn.socket
floppa status
```

Membership of `floppa` means being able to change this machine's default route, which is
equivalent to root for every purpose that matters here. It is an administrator's decision, which
is why installing the package does not make it for you. The Debian package enables the socket;
the Arch package does not, following each distribution's convention.

Nothing starts until a client connects. `floppa-vpn.service` has no `[Install]` section and is
reached only through the socket.

### The files

| file | why |
|---|---|
| `/usr/lib/systemd/system/floppa-vpn.socket` | owns the socket, so the program does not have to. A unit states `SocketGroup` and `SocketMode` and the file exists with them *before* the service starts; a program that binds and then widens its own socket has a window where it is listening and reachable by the wrong people |
| `/usr/lib/systemd/system/floppa-vpn.service` | the tunnel process. No `RuntimeDirectory=`, deliberately: systemd removes one when the service stops, and it would take the socket out from under the unit that outlives it |
| `/usr/lib/systemd/system/floppa-vpn-autostart.service` | asks the service to restore the last successful connection on boot when `floppa autostart` is `on`; enabled during Debian installation, explicitly by the administrator on Arch |
| `/usr/lib/tmpfiles.d/floppa-vpn.conf` | `/run/floppa-vpn` as `0750 root:floppa`, so a user outside the group cannot reach the path at all, whatever the socket inside it says |
| `/usr/lib/sysusers.d/floppa-vpn.conf` | the `floppa` group, created empty |

## What it does with what

**State** lives in `/var/lib/floppa-vpn`, `0700`, created by `StateDirectory=`: the config store,
the rollback journal, and the server session. Not a keyring — a root process has no D-Bus session,
so every keyring call is a blocking round trip that fails, once on load and once on *every* save.
`config::use_file_storage_only()` makes the file the storage rather than the fallback. It is the
same reason `wg-quick` keeps configs in `/etc/wireguard` rather than in anybody's keyring: a VPN
that must come up before anyone has logged in cannot depend on one.

**Credentials** travel from the client to the service and never back. The peer watcher runs here,
so it can replace a peer the server deleted with nobody looking — but reaching the server needs a
token that belongs to a user, and keeping it safe needs a place that belongs to root. So
`floppa connect` hands the session over (`VpnRpc::set_session`), and the payload crosses **opaque**:
`floppa-vpn-core` runs tunnels and is not told what a server session is. There is no call to read
one back out, and there is not going to be one. What the socket grants is the *use* of an account,
never a copy of the token.

**The wire** is versioned, which the Android one never needed to be. There both ends ship in one
APK and installing either replaces the other, so two builds are never live at once. Upgrading a
package does not restart a running service, so a new client talking to an old one is the ordinary
outcome of an update — every answer to `state_since` states the protocol version, and a mirror that
does not recognise it declines the state rather than adopting it.

## `connect` has two shapes

Which one runs is decided by what the caller supplied, not by a switch.

```bash
floppa connect                                   # ask the service; it keeps the tunnel after this exits
floppa status
floppa disconnect

sudo floppa connect --config ./wg0.conf          # build one here and hold it until interrupted
```

`--config` means "here is a tunnel to build", and such a run must not touch the machine's
long-lived one. It is also what `tests/integration/conftest.py` runs, in a container with no
service in it, which is another reason the flag decides rather than a mode switch.

## Connecting on boot

```bash
sudo systemctl enable floppa-vpn-autostart.service  # required on Arch; Debian enables it during installation
floppa autostart on      # or the switch in the app's settings
floppa autostart         # prints on/off
```

First enable the socket and apply group membership as described above, then connect successfully
through the GUI or `floppa connect`. The boot unit restores that connection without opening the GUI.

`floppa-vpn-autostart.service` is a `oneshot` running `floppa resume --if-enabled`.
Both the unit and the stored preference must be enabled. Once the unit is enabled, use
`floppa autostart off` to stop connecting on boot; this does not disconnect a running tunnel.

The unit runs after `network-online.target`, which only waits for network setup when the
network manager's matching wait-online service is enabled. On a NetworkManager system:

```bash
sudo systemctl enable NetworkManager-wait-online.service
```

For another network manager, use its wait-online service instead. Package installation does not
enable one because it cannot assume which network manager the machine uses. NetworkManager can
still finish startup without a usable connection, for example when Wi-Fi credentials become
available only after login. If a connection attempt fails, the boot unit retries every five
seconds until it succeeds, even after the wait-online timeout. A disabled preference or no previous
successful connection exits normally without retries.

To check setup and diagnose a boot failure:

```bash
systemctl is-enabled floppa-vpn.socket floppa-vpn-autostart.service
floppa autostart
floppa status
sudo journalctl -b -u floppa-vpn-autostart.service -u floppa-vpn.service
```

That is not the shape anyone would pick. `systemctl is-enabled` is exactly where a person would
look, and this deliberately does not answer there. The reason is a limit in systemd, verified
against polkit 127 rather than assumed: it passes polkit the unit's name for `manage-units` (start,
stop) but **not** for `manage-unit-files` (enable, disable). A rule scoped to this one unit
therefore matches nothing, and the only rule that would work grants the `floppa` group the right to
enable *any* unit — which is root under another name. The alternative is an administrator password
prompt on the switch, which would put a setting behind an authority that has nothing to do with
whether someone may run this VPN: group membership is what gates everything else a client does
here, and connecting on boot belongs inside it. So the preference lives where that authority
reaches, in the service's own state directory, and the unit asks for it.

The actor owns `runtime.json` v4 alongside `settings.json`. It stores durable desired
Up/Down, boot identity, last successful protocol and server-granted routes. In the
same boot, a restarted long-lived owner recovers desired Up after network cleanup.
On a new boot it consults `startup.connect_on_boot`. Explicit Disconnect persists
Down; service shutdown uses Suspend so a restart does not become a user disconnect.
Short-lived standalone CLI queries disable automatic resume. A standalone process
still needs to be launched again before it can recover.

`connect --config` supplies an ephemeral profile to the selected owner, either
service or standalone. It is neither imported nor recorded for restart. `floppa
import` explicitly persists credentials. All normal connection requests derive
preferences from the selected owner's settings document.

Settings writes publish a new revision and reconcile the active configured tunnel.
Subscribers receive saved and applied revisions separately. GUI and CLI observe
`failed` and `needs_attention` rather than treating failed cleanup as disconnected.
Linux uses the privileged helper's shared rollback directory; Windows uses a
protected Administrators/SYSTEM directory in ProgramData. See
[the architecture](CONNECTION-ARCHITECTURE.md) for current paths and validation limits.

## Not done yet

**Hardening the unit.** It runs as root with no `ProtectSystem=` or capability bounding. Narrowing
it needs testing against the DNS path, which rewrites `/etc/resolv.conf`.

The machine-wide ownership lock now covers service and standalone, including
custom interface names. A competing owner fails before touching the network.

**Windows** needs a named pipe instead of a Unix socket and a service instead of a unit. Deferred:
the tray already keeps the tunnel alive while the app is open there.

## Diagnostic collection

Diagnostic capture was primarily built for Android, where exporting app and VPN
process logs from the GUI avoids needing adb. It can also collect GUI logs on desktop.
The Linux system tunnel service logs to journald, independently of GUI capture:

```sh
journalctl -u floppa-vpn.service --since "30 minutes ago" --no-pager > floppa-vpn-service.log
```

GUI export does not currently include these journal entries. This is a difference
in collection methods, not missing service logs. Combining them into one support
archive is optional convenience work, not a requirement for service support.
See [the client backlog](CLIENT-BACKLOG.md).

## Troubleshooting

**"this user may not use it"** — you are not in the `floppa` group, or you are but have not logged
in again since. `id -nG` will tell you which.

**"the tunnel service speaks protocol N"** — the running service is the binary from before an
upgrade. `sudo systemctl restart floppa-vpn.service`. The Arch package does this for you on
upgrade when the service is running.

**Service absent** — GUI and CLI select standalone automatically. An inaccessible,
incompatible or unresponsive service is an explicit error; it never triggers a
second owner. `--config` selects an ephemeral input profile, not the execution mode.

**Watching it** — `journalctl -u floppa-vpn.service -f`. The service logs to stderr, which
journald captures; `RUST_LOG` applies as it does to any other run.
