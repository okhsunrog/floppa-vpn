#!/bin/sh
set -eu

for bin in /usr/bin/floppa-client /usr/bin/floppa; do
  setcap cap_net_admin+ep "$bin" || {
    echo "floppa-vpn: cannot set CAP_NET_ADMIN on $bin; standalone mode requires it" >&2
    exit 1
  }
done

# The tunnel service. Each step is guarded: a container or a chroot may have none of this, and an
# install must not fail because a system has no systemd to tell.
#
# The `floppa` group is created empty and nobody is put in it. Being in it means being able to
# change this machine's default route, which is a decision for whoever administers the machine —
# `usermod -aG floppa <user>` is the documented next step, not something a package does.
if command -v systemd-sysusers >/dev/null 2>&1; then
  systemd-sysusers || echo "floppa-vpn: could not create the floppa group" >&2
fi
if command -v systemd-tmpfiles >/dev/null 2>&1; then
  systemd-tmpfiles --create /usr/lib/tmpfiles.d/floppa-vpn.conf || true
fi

if command -v systemctl >/dev/null 2>&1 && [ -d /run/systemd/system ]; then
  systemctl daemon-reload || true
  # The socket costs nothing while nobody connects — the service starts on demand — and leaving it
  # disabled would mean the feature exists and does nothing until somebody reads a document. The
  # tunnel service has no [Install] of its own; the socket is what starts it.
  systemctl enable --now floppa-vpn.socket || \
    echo "floppa-vpn: could not enable floppa-vpn.socket" >&2
  # The boot unit is enabled too, and does nothing until `floppa autostart on`. Its enabled-ness
  # is not the setting — systemd will not let the `floppa` group flip that without an admin
  # password — so it stays on and reads the preference instead.
  systemctl enable floppa-vpn-autostart.service || \
    echo "floppa-vpn: could not enable floppa-vpn-autostart.service" >&2
fi

exit 0
