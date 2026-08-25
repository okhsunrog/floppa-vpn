#!/bin/bash
# Install Floppa VPN polkit policy and network helper
#
# Run as root: sudo ./install-polkit.sh
# Uninstall:   sudo ./install-polkit.sh --uninstall

set -euo pipefail

HELPER_DEST="/usr/lib/floppa-vpn/floppa-network-helper"
POLICY_DEST="/usr/share/polkit-1/actions/dev.okhsunrog.floppa-vpn.policy"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

if [[ "${1:-}" == "--uninstall" ]]; then
    echo "Removing Floppa VPN polkit policy..."
    rm -f "$POLICY_DEST"
    rm -f "$HELPER_DEST"
    rmdir /usr/lib/floppa-vpn 2>/dev/null || true
    echo "Done."
    exit 0
fi

if [[ $EUID -ne 0 ]]; then
    echo "This script must be run as root (sudo)." >&2
    exit 1
fi

echo "Installing Floppa VPN polkit policy..."

mkdir -p /usr/lib/floppa-vpn
install -m 755 "$SCRIPT_DIR/floppa-network-helper" "$HELPER_DEST"
install -m 644 "$SCRIPT_DIR/dev.okhsunrog.floppa-vpn.policy" "$POLICY_DEST"

echo "Installed:"
echo "  $HELPER_DEST"
echo "  $POLICY_DEST"
echo ""
echo "VPN connect/disconnect will now work without a password prompt."
