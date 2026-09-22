# The desktop tunnel service

The Linux counterpart of the Android `:vpn` split, and for a related reason. On Android the UI
process is frozen in the background, so an actor living there cannot reconnect a tunnel that died
while the phone was in a pocket. On a desktop the UI process is not frozen — it is *closed*, and
the tunnel goes with it.

`floppa service` is the process that holds it instead. The same binary as the command-line client,
started a different way.

## Three ways a tunnel gets onto a machine

Not two. The service is the one to prefer where it exists; the others are what every other
platform has, and what a tarball or an AppImage has on Linux.

| mode | holds the actor | how it gets privileges | store | when |
|---|---|---|---|---|
| **service** | `floppa service`, as root | it *is* root | `/var/lib/floppa-vpn`, `0700` | Linux with the package |
| **in-process, unprivileged** | the app, or `floppa` as a user | `pkexec` per change | the OS keyring | tarball, AppImage, Windows, macOS, socket disabled |
| **in-process, root** | `sudo floppa --config …` | already root | nothing persisted | one-shot runs, the test containers |

The second row is what the desktop app has always done, unchanged. It is also the only mode on
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

Four answers, and the difference between two of them is the point:

- **Available** — drive the service.
- **Absent** — nothing is listening. Run the tunnel in this process.
- **Forbidden** — a service *is* there and this user may not talk to it. This is not absence, and
  treating it as absence is the trap: falling back would work, polkit would ask for a password,
  and a tunnel would come up. It would then work slightly worse forever — a prompt on every
  connect, a tunnel that still dies with the program — for a reason nobody would ever be shown.
  So it is shown: you are not in the `floppa` group.
- **WrongVersion** — a service is there speaking a protocol this build does not. Almost always an
  upgrade that replaced the binaries while the old service kept running. Restart it.

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
| `/usr/lib/systemd/system/floppa-vpn-autostart.service` | connecting on boot. Not enabled by anything; enabling it *is* the setting |
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
floppa autostart on      # or the switch in the app's settings
floppa autostart         # prints on/off
```

`floppa-vpn-autostart.service` is a `oneshot` running `floppa resume --if-enabled`, and the package
enables it. **Its enabled-ness is not the setting** — it stays on and does nothing until asked.

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

`resume` is a *client* command for a separate reason: the tunnel service is started by its socket
as well as at boot, so a service that reconnected whenever it started would reconnect every time
anybody ran `floppa status`, on a VPN they had just turned off. Being **asked** is not the same as
being **started**, and only a caller can tell the two apart.

What it resumes is not named in the unit. The service writes down what last connected — the winner
first, since that is the protocol that actually carried a tunnel — into `autostart.json` beside the
configs, which only root can read. So the request is "whatever you had", and the process that knows
answers it. A machine that has never connected is told so, and nothing happens.

The same file is what Android uses for always-on and boot starts; the difference is only who does
the asking.

## Handing it a config you already have

```bash
floppa import ./wg0.conf     # into the service's store
floppa connect               # uses it
```

`connect --config` deliberately does not do this — that flag means "build a tunnel in this
command" — so `import` is the way into a store that belongs to root. With a config there and nobody
logged in, `floppa connect` uses it rather than sending you to the server for one you already have:
running a WireGuard tunnel does not require a Floppa account.

## Not done yet

**The desktop app still runs its own actor** when there is no service, which is correct, but it has
no switch for connecting on boot — that is `systemctl enable` for now. A GUI toggle needs polkit or
a preference carried over the socket, and is a piece of its own.

**Windows** needs a named pipe instead of a Unix socket and a service instead of a unit. Deferred:
the tray already keeps the tunnel alive while the app is open there.

**Hardening the unit.** It runs as root with no `ProtectSystem=` or capability bounding. Narrowing
it is worthwhile and needs testing against the DNS path, which rewrites `/etc/resolv.conf`.

## Troubleshooting

**"this user may not use it"** — you are not in the `floppa` group, or you are but have not logged
in again since. `id -nG` will tell you which.

**"the tunnel service speaks protocol N"** — the running service is the binary from before an
upgrade. `sudo systemctl restart floppa-vpn.service`. The Arch package does this for you on
upgrade when the service is running.

**"no tunnel service is running"** — the socket is not enabled, or this is not a packaged install.
`systemctl enable --now floppa-vpn.socket`, or use `sudo floppa connect --config <file>`.

**Watching it** — `journalctl -u floppa-vpn.service -f`. The service logs to stderr, which
journald captures; `RUST_LOG` applies as it does to any other run.
