#!/bin/sh
set -eu

BIN_PATH="/usr/bin/floppa-client"

if ! command -v setcap >/dev/null 2>&1; then
  echo "floppa-vpn: setcap not found; skipping CAP_NET_ADMIN setup" >&2
  exit 0
fi

if [ ! -x "$BIN_PATH" ]; then
  echo "floppa-vpn: binary not found at $BIN_PATH; skipping CAP_NET_ADMIN setup" >&2
  exit 0
fi

if ! setcap cap_net_admin+ep "$BIN_PATH"; then
  echo "floppa-vpn: failed to set CAP_NET_ADMIN on $BIN_PATH" >&2
  exit 0
fi

exit 0
