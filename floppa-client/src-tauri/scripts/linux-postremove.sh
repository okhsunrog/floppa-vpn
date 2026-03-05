#!/bin/sh
set -eu

BIN_PATH="/usr/bin/floppa-client"

if ! command -v setcap >/dev/null 2>&1; then
  exit 0
fi

if [ -e "$BIN_PATH" ]; then
  setcap -r "$BIN_PATH" || true
fi

exit 0
