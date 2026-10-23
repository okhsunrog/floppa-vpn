#!/usr/bin/env bash
# Receives the actual production route calculation from the Rust integration test.
set -euo pipefail
[[ "$(readlink /proc/self/ns/net)" != "$(readlink /proc/1/ns/net)" ]] || exit 1
[[ "$(readlink /proc/self/ns/mnt)" != "$(readlink /proc/1/ns/mnt)" ]] || exit 1
mount --make-rprivate /
mount -t tmpfs tmpfs /var/lib
helper="$(dirname "$0")/../../target/debug/floppa-network-helper"
for dev in wifi ethernet floppa0 corp0; do
    ip link add "$dev" type dummy
    ip link set "$dev" up
done
ip addr add 192.0.2.2/24 dev wifi
ip addr add 198.51.100.2/24 dev ethernet
ip addr add 10.101.0.2/24 dev floppa0
ip addr add 172.21.0.2/24 dev corp0
ip route add default via 192.0.2.1 dev wifi metric 600
ip route add default via 198.51.100.1 dev ethernet metric 100
# Corporate routes precede Floppa in one ordering, follow it in the other.
for order in before after; do
    if [[ "$order" == before ]]; then ip route add 10.20.0.0/16 dev corp0; fi
    "$helper" add-routes floppa0 "$@"
    "$helper" add-policy 1 0
    if [[ "$order" == after ]]; then ip route add 10.20.0.0/16 dev corp0; fi
    ip route get 194.186.63.1 | grep -q 'dev ethernet'
    ip route get 1.1.1.1 | grep -q 'dev floppa0'
    ip route get 10.20.0.1 | grep -q 'dev corp0'
    # Only change the physical defaults; never rewrite any Floppa route.
    ip route del default via 198.51.100.1 dev ethernet metric 100
    ip route get 194.186.63.1 | grep -q 'dev wifi'
    ip route add default via 198.51.100.1 dev ethernet metric 100
    ip route get 194.186.63.1 | grep -q 'dev ethernet'
    # Cleanup uses the same journal-backed Rust helper as the actor.
    "$helper" recover-policy
    "$helper" recover-routes
    ip route get 1.1.1.1 | grep -q 'dev ethernet'
    ip route get 10.20.0.1 | grep -q 'dev corp0'
    ip route del 10.20.0.0/16 dev corp0
done
[[ "$(ip -4 rule show | wc -l)" -eq 3 ]]
echo 'Exclusions, handover, coexistence and cleanup passed'
