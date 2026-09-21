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

The package ships the binary and four files, and puts nobody in the group.

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

## Not done yet

**Reconnecting after a reboot.** The service holds the tunnel across a client closing, but nothing
brings one back at boot. That needs a stated preference — "connect on boot" as a thing the user
turned on — rather than "whatever was up last", so it arrives with the toggle that sets it. The
`[Install]` section of the service unit arrives then too.

**The desktop app still runs its own actor.** It will use the service the same way the
command-line client does.

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
