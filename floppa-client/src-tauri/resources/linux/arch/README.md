# Arch Packaging

This directory contains Arch Linux packaging files for `floppa-vpn`:

- `PKGBUILD`
- `floppa-vpn.install`

Build and install locally:

```bash
cd floppa-client/src-tauri/resources/linux/arch
makepkg -si
```

`floppa-vpn.install` applies `CAP_NET_ADMIN` to `/usr/bin/floppa-client` on install/upgrade
so Linux `fwmark` can work without runtime fallback.

Dependency references used when defining `depends`/`makedepends`:

- Tauri AUR packaging docs: https://v2.tauri.app/distribute/aur/
- Tauri Linux prerequisites: https://v2.tauri.app/start/prerequisites/#linux
