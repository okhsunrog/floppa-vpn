# Arch Packaging

This directory contains Arch Linux packaging files for `floppa-vpn`:

- `PKGBUILD` — release build from tagged GitHub source tarball. **This is the one CI builds
  for a release**, so a change here reaches the package on the release page.
- `PKGBUILD-git` — builds from latest git HEAD
- `PKGBUILD-local` — builds from local working tree (fastest for dev)
- `floppa-vpn.install` — pacman install hooks (sets `CAP_NET_ADMIN`)

## Usage

Release build:

```bash
cd scripts/arch
makepkg -si
```

Build from git HEAD:

```bash
cd scripts/arch
makepkg -p PKGBUILD-git -si
```

Build from local tree (no download):

```bash
cd scripts/arch
makepkg -p PKGBUILD-local -si
```

`floppa-vpn.install` applies `CAP_NET_ADMIN` to `/usr/bin/floppa-client` and
`/usr/bin/floppa` after install/upgrade. Package archives do not preserve these
file capabilities, so the install hook assigns them to the installed files.
Every Linux transport socket must accept `SO_MARK`; failure stops the connection
with an error. Both standalone entry points therefore need the capability.
No socket broker or extra privileged process is introduced.

## Tunnel service and boot autostart

The package includes the GUI (`floppa-client`), CLI (`floppa`), and systemd units.
Arch installation does not enable the units or add users to the `floppa` group.

```bash
sudo usermod -aG floppa "$USER"
sudo systemctl enable --now floppa-vpn.socket
```

Log out and back in to apply group membership, then reopen the GUI and connect, or run
`floppa login` followed by `floppa connect`. The system service keeps the tunnel running
after the client closes.

After the first successful connection, enable boot autostart:

```bash
sudo systemctl enable floppa-vpn-autostart.service
floppa autostart on
```

On systems using NetworkManager, also enable network startup waiting:

```bash
sudo systemctl enable NetworkManager-wait-online.service
```

Use the matching wait-online service for other network managers. The boot unit retries failed
connection attempts every five seconds if the network is still unavailable. It restores the last
successful connection without launching the GUI. `floppa autostart off` disables boot connection;
`floppa disconnect` stops the current tunnel.

See [the desktop tunnel service guide](../../docs/DESKTOP-TUNNEL-SERVICE.md#connecting-on-boot)
for status commands and boot logs.

Dependency references:

- Tauri AUR packaging docs: https://v2.tauri.app/distribute/aur/
- Tauri Linux prerequisites: https://v2.tauri.app/start/prerequisites/#linux
