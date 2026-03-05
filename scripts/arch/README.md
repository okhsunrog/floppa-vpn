# Arch Packaging

This directory contains Arch Linux packaging files for `floppa-vpn`:

- `PKGBUILD` — release build from tagged GitHub source tarball
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

`floppa-vpn.install` applies `CAP_NET_ADMIN` to `/usr/bin/floppa-client` on install/upgrade
so Linux `fwmark` can work without runtime fallback.

Dependency references:

- Tauri AUR packaging docs: https://v2.tauri.app/distribute/aur/
- Tauri Linux prerequisites: https://v2.tauri.app/start/prerequisites/#linux
