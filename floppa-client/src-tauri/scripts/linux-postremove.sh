#!/bin/sh
set -eu

BIN_PATH="/usr/bin/floppa-client"

if command -v setcap >/dev/null 2>&1 && [ -e "$BIN_PATH" ]; then
  setcap -r "$BIN_PATH" || true
fi

# Stop holding the tunnel. Both, and in this order: stopping only the service would leave the
# socket to start it again the moment anything connected.
#
# The `floppa` group is left behind on purpose. Removing it would renumber nothing but would strip
# a membership an administrator granted, and a reinstall would then silently not work for the
# person who had set it up.
if command -v systemctl >/dev/null 2>&1 && [ -d /run/systemd/system ]; then
  systemctl disable --now floppa-vpn.socket >/dev/null 2>&1 || true
  systemctl stop floppa-vpn.service >/dev/null 2>&1 || true
  systemctl daemon-reload || true
fi

exit 0
