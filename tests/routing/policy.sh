#!/usr/bin/env bash
# Run only in a fresh network namespace: sudo unshare --net bash tests/routing/policy.sh
set -euo pipefail
[[ "$(readlink /proc/self/ns/net)" != "$(readlink /proc/1/ns/net)" ]] || { echo 'Refusing host network namespace' >&2; exit 1; }
helper="$(cd "$(dirname "$0")/../.." && pwd)/floppa-vpn-core/resources/linux/floppa-network-helper"
ip link add uplink type dummy
ip addr add 192.0.2.2/24 dev uplink
ip link set uplink up
ip link add floppa0 type dummy
ip addr add 10.101.0.2/24 dev floppa0
ip link set floppa0 up
ip link add corp0 type dummy
ip addr add 172.21.0.2/24 dev corp0
ip link set corp0 up
ip route add default via 192.0.2.1 dev uplink
ip route add 0.0.0.0/1 dev corp0
ip route add 128.0.0.0/1 dev corp0
bash "$helper" check-policy 52600 90
bash "$helper" add-policy 194.186.63.0/24 52600 90 uplink 192.0.2.1
bash "$helper" add-policy 160.79.104.0/22 52601 91 floppa0 -
ip route get 194.186.63.1 | grep -q 'dev uplink'
ip route get 160.79.104.1 | grep -q 'dev floppa0'
ip route get 8.8.8.8 | grep -q 'dev corp0'
# Occupied slots cannot be silently overwritten.
if bash "$helper" check-policy 52600 90; then exit 1; fi
# More specific system exception wins over a broad tunnel rule regardless of insertion order.
bash "$helper" add-policy 0.0.0.0/0 52602 93 floppa0 -
bash "$helper" add-policy 203.0.113.0/24 254 92 - -
ip route get 203.0.113.5 | grep -q 'dev corp0'
ip route get 8.8.8.8 | grep -q 'dev floppa0'
# The transport endpoint must stay out of the tunnel even with a catch-all rule.
ip route add 198.51.100.7/32 via 192.0.2.1 dev uplink
bash "$helper" add-policy 198.51.100.7/32 254 89 - -
ip route get 198.51.100.7 | grep -q 'dev uplink'
bash "$helper" del-policy 198.51.100.7/32 254 89 - -
bash "$helper" del-policy 203.0.113.0/24 254 92 - -
bash "$helper" del-policy 0.0.0.0/0 52602 93 floppa0 -
# A vanished selected interface must not silently send its traffic through the other VPN.
ip link del floppa0
if ip route get 160.79.104.1 2>/dev/null; then exit 1; fi
# Foreign routes are preserved even inside our numeric table range.
ip route add table 52600 203.0.113.0/24 dev corp0 proto static
# Exact cleanup is idempotent and leaves the other VPN untouched.
for n in 1 2; do
 bash "$helper" del-policy 194.186.63.0/24 52600 90 uplink 192.0.2.1
 bash "$helper" del-policy 160.79.104.0/22 52601 91 floppa0 -
done
ip route get 194.186.63.1 | grep -q 'dev corp0'
ip route show table 52600 | grep -q "203.0.113.0/24 dev corp0"
[[ "$(ip route show table 52600 | wc -l)" -eq 1 ]]
[[ -z "$(ip route show table 52601 2>/dev/null || true)" ]]
echo 'routing policy namespace tests passed'
