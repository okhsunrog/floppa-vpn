"""Tests for tc HFSC speed limiting on a local WireGuard tunnel.

Creates two Docker containers (server + client) connected via WireGuard,
applies the exact same tc commands as floppa-daemon, and measures throughput
with iperf3 to verify rate limiting works.
"""

import json
import time
import uuid

import pytest

from conftest import (
    DOCKER_IMAGE,
    _start_container_on_network,
    _stop_container,
    _wait_for_handshake,
    docker_exec,
    docker_exec_detach,
    docker_network_create,
    docker_network_remove,
    generate_wg_keypair,
    get_container_ip,
)

# VPN subnet for local testing
SERVER_VPN_IP = "10.200.0.1"
CLIENT_VPN_IP = "10.200.0.2"
VPN_SUBNET = "24"
WG_PORT = "51820"
WG_IFACE = "wg0"
IFB_DEVICE = "ifb-test"
TOTAL_BANDWIDTH_MBIT = 1000
IPERF_DURATION = 5  # seconds


def _run_iperf3(client_container: str, server_ip: str, reverse: bool = False) -> float:
    """Run iperf3 and return bandwidth in Mbps.

    Args:
        client_container: Docker container to run iperf3 client in
        server_ip: IP address of the iperf3 server
        reverse: If True, measure server→client (download) direction

    Returns:
        Bandwidth in Mbps
    """
    cmd = ["iperf3", "-c", server_ip, "-t", str(IPERF_DURATION), "-J"]
    if reverse:
        cmd.append("-R")
    result = docker_exec(client_container, cmd, timeout=IPERF_DURATION + 10)
    data = json.loads(result.stdout)
    bits_per_second = data["end"]["sum_received"]["bits_per_second"]
    return bits_per_second / 1_000_000  # Convert to Mbps


def _setup_tc_infrastructure(container: str, interface: str = WG_IFACE) -> None:
    """Apply the same tc setup as floppa-daemon's setup_tc().

    Reproduces crates/floppa-daemon/src/tc.rs::setup_tc() exactly.
    """
    rate = f"{TOTAL_BANDWIDTH_MBIT}mbit"

    # === EGRESS setup ===
    docker_exec(container, [
        "tc", "qdisc", "add", "dev", interface, "root",
        "handle", "1:", "hfsc", "default", "99",
    ])
    docker_exec(container, [
        "tc", "class", "add", "dev", interface, "parent", "1:",
        "classid", "1:1", "hfsc", "sc", "rate", rate, "ul", "rate", rate,
    ])
    docker_exec(container, [
        "tc", "class", "add", "dev", interface, "parent", "1:1",
        "classid", "1:99", "hfsc", "ls", "rate", rate,
    ])

    # === INGRESS setup via IFB ===
    docker_exec(container, ["modprobe", "ifb"], check=False)  # may already be loaded
    docker_exec(container, [
        "ip", "link", "add", "name", IFB_DEVICE, "type", "ifb",
    ])
    docker_exec(container, ["ip", "link", "set", IFB_DEVICE, "up"])
    docker_exec(container, [
        "tc", "qdisc", "add", "dev", interface, "handle", "ffff:", "ingress",
    ])
    docker_exec(container, [
        "tc", "filter", "add", "dev", interface, "parent", "ffff:",
        "matchall", "action", "mirred", "egress", "redirect", "dev", IFB_DEVICE,
    ])
    docker_exec(container, [
        "tc", "qdisc", "add", "dev", IFB_DEVICE, "root",
        "handle", "1:", "hfsc", "default", "99",
    ])
    docker_exec(container, [
        "tc", "class", "add", "dev", IFB_DEVICE, "parent", "1:",
        "classid", "1:1", "hfsc", "sc", "rate", rate, "ul", "rate", rate,
    ])
    docker_exec(container, [
        "tc", "class", "add", "dev", IFB_DEVICE, "parent", "1:1",
        "classid", "1:99", "hfsc", "ls", "rate", rate,
    ])


def _add_peer_limit(container: str, peer_ip: str, rate_mbit: int) -> None:
    """Apply per-peer rate limit — same as tc.rs::add_peer_limit()."""
    # Class ID from last octet (peer is on 10.200.0.x, so third=0, fourth=x)
    fourth = int(peer_ip.split(".")[-1])
    classid = f"1:{fourth}"
    rate = f"{rate_mbit}mbit"
    dst = f"{peer_ip}/32"
    src = f"{peer_ip}/32"

    # Egress class + filter
    docker_exec(container, [
        "tc", "class", "add", "dev", WG_IFACE, "parent", "1:1",
        "classid", classid, "hfsc", "ls", "rate", rate, "ul", "rate", rate,
    ])
    docker_exec(container, [
        "tc", "filter", "add", "dev", WG_IFACE, "parent", "1:",
        "protocol", "ip", "prio", "1", "u32",
        "match", "ip", "dst", dst, "classid", classid,
    ])

    # Ingress class + filter (on IFB)
    docker_exec(container, [
        "tc", "class", "add", "dev", IFB_DEVICE, "parent", "1:1",
        "classid", classid, "hfsc", "ls", "rate", rate, "ul", "rate", rate,
    ])
    docker_exec(container, [
        "tc", "filter", "add", "dev", IFB_DEVICE, "parent", "1:",
        "protocol", "ip", "prio", "1", "u32",
        "match", "ip", "src", src, "classid", classid,
    ])


def _update_peer_limit(container: str, peer_ip: str, rate_mbit: int) -> None:
    """Update per-peer rate limit — same as tc.rs::update_peer_limit()."""
    fourth = int(peer_ip.split(".")[-1])
    classid = f"1:{fourth}"
    rate = f"{rate_mbit}mbit"

    docker_exec(container, [
        "tc", "class", "change", "dev", WG_IFACE, "parent", "1:1",
        "classid", classid, "hfsc", "ls", "rate", rate, "ul", "rate", rate,
    ])
    docker_exec(container, [
        "tc", "class", "change", "dev", IFB_DEVICE, "parent", "1:1",
        "classid", classid, "hfsc", "ls", "rate", rate, "ul", "rate", rate,
    ])


def _remove_peer_limit(container: str, peer_ip: str) -> None:
    """Remove per-peer rate limit — same as tc.rs::remove_peer_limit()."""
    fourth = int(peer_ip.split(".")[-1])
    classid = f"1:{fourth}"
    dst = f"{peer_ip}/32"
    src = f"{peer_ip}/32"

    # Remove egress filter + class
    docker_exec(container, [
        "tc", "filter", "del", "dev", WG_IFACE, "parent", "1:",
        "protocol", "ip", "prio", "1", "u32",
        "match", "ip", "dst", dst,
    ], check=False)
    docker_exec(container, [
        "tc", "class", "del", "dev", WG_IFACE, "parent", "1:1",
        "classid", classid,
    ], check=False)

    # Remove ingress filter + class from IFB
    docker_exec(container, [
        "tc", "filter", "del", "dev", IFB_DEVICE, "parent", "1:",
        "protocol", "ip", "prio", "1", "u32",
        "match", "ip", "src", src,
    ], check=False)
    docker_exec(container, [
        "tc", "class", "del", "dev", IFB_DEVICE, "parent", "1:1",
        "classid", classid,
    ], check=False)


def _cleanup_tc(container: str) -> None:
    """Remove all tc rules — same as tc.rs::cleanup_tc()."""
    docker_exec(container, ["tc", "qdisc", "del", "dev", WG_IFACE, "root"], check=False)
    docker_exec(container, ["tc", "qdisc", "del", "dev", WG_IFACE, "ingress"], check=False)
    docker_exec(container, ["ip", "link", "del", IFB_DEVICE], check=False)


# ── Fixtures ──────────────────────────────────────────────────────────────────


@pytest.fixture(scope="module")
def docker_image():
    """Build the Docker image."""
    import subprocess
    from conftest import INTEGRATION_DIR
    subprocess.run(
        ["docker", "build", "-t", DOCKER_IMAGE, str(INTEGRATION_DIR)],
        check=True,
        capture_output=True,
    )
    return DOCKER_IMAGE


@pytest.fixture(scope="module")
def wg_tunnel(docker_image):
    """Set up two Docker containers with a WireGuard tunnel between them.

    Yields (server_container_name, client_container_name).
    """
    net_name = f"floppa-tc-net-{uuid.uuid4().hex[:8]}"
    server_name = f"floppa-tc-server-{uuid.uuid4().hex[:8]}"
    client_name = f"floppa-tc-client-{uuid.uuid4().hex[:8]}"

    docker_network_create(net_name)

    try:
        _start_container_on_network(docker_image, server_name, net_name)
        _start_container_on_network(docker_image, client_name, net_name)

        # Generate keypairs
        server_privkey, server_pubkey = generate_wg_keypair(server_name)
        client_privkey, client_pubkey = generate_wg_keypair(client_name)

        # Get Docker network IPs for WG endpoints
        server_docker_ip = get_container_ip(server_name, net_name)

        # Configure server WG interface
        docker_exec(server_name, ["ip", "link", "add", WG_IFACE, "type", "wireguard"])
        docker_exec(server_name, [
            "sh", "-c",
            f"echo '{server_privkey}' > /tmp/wg_privkey && wg set {WG_IFACE} "
            f"listen-port {WG_PORT} private-key /tmp/wg_privkey "
            f"peer {client_pubkey} allowed-ips {CLIENT_VPN_IP}/32",
        ])
        docker_exec(server_name, [
            "ip", "addr", "add", f"{SERVER_VPN_IP}/{VPN_SUBNET}", "dev", WG_IFACE,
        ])
        docker_exec(server_name, ["ip", "link", "set", WG_IFACE, "up"])

        # Configure client WG interface
        docker_exec(client_name, ["ip", "link", "add", WG_IFACE, "type", "wireguard"])
        docker_exec(client_name, [
            "sh", "-c",
            f"echo '{client_privkey}' > /tmp/wg_privkey && wg set {WG_IFACE} "
            f"private-key /tmp/wg_privkey "
            f"peer {server_pubkey} allowed-ips {SERVER_VPN_IP}/32 "
            f"endpoint {server_docker_ip}:{WG_PORT} persistent-keepalive 1",
        ])
        docker_exec(client_name, [
            "ip", "addr", "add", f"{CLIENT_VPN_IP}/{VPN_SUBNET}", "dev", WG_IFACE,
        ])
        docker_exec(client_name, ["ip", "link", "set", WG_IFACE, "up"])

        # Trigger handshake from client
        docker_exec(client_name, ["ping", "-c", "1", "-W", "5", SERVER_VPN_IP], check=False)
        _wait_for_handshake(client_name, WG_IFACE, timeout=10)

        # Start iperf3 server on the server container (bound to VPN IP)
        docker_exec_detach(server_name, ["iperf3", "-s", "-B", SERVER_VPN_IP])
        time.sleep(0.5)  # let iperf3 start

        yield server_name, client_name

    finally:
        _stop_container(server_name)
        _stop_container(client_name)
        docker_network_remove(net_name)


# ── Tests ─────────────────────────────────────────────────────────────────────


class TestSpeedLimit:
    """Test tc HFSC speed limiting on a WireGuard tunnel."""

    def test_tunnel_connectivity(self, wg_tunnel):
        """Sanity check: ping through the WireGuard tunnel."""
        server, client = wg_tunnel
        result = docker_exec(client, ["ping", "-c", "3", "-W", "5", SERVER_VPN_IP])
        assert "0% packet loss" in result.stdout

    def test_baseline_bandwidth(self, wg_tunnel):
        """Measure baseline bandwidth without any tc rules."""
        server, client = wg_tunnel
        mbps = _run_iperf3(client, SERVER_VPN_IP)
        print(f"\nBaseline bandwidth: {mbps:.1f} Mbps")
        # Should be high (hundreds of Mbps on localhost Docker)
        assert mbps > 50, f"Baseline too low: {mbps:.1f} Mbps"

    def test_egress_rate_limit(self, wg_tunnel):
        """Apply 10 Mbps egress limit and verify bandwidth is capped.

        Egress on server = traffic TO client = client download.
        """
        server, client = wg_tunnel
        limit_mbit = 10

        try:
            _setup_tc_infrastructure(server)
            _add_peer_limit(server, CLIENT_VPN_IP, limit_mbit)

            # Measure download (server→client): use -R (reverse) so server sends
            mbps = _run_iperf3(client, SERVER_VPN_IP, reverse=True)
            print(f"\nEgress limited to {limit_mbit} Mbps, measured: {mbps:.1f} Mbps")

            # Allow 30% tolerance above limit
            max_expected = limit_mbit * 1.3
            assert mbps <= max_expected, (
                f"Egress rate limit not working: expected <={max_expected:.0f} Mbps, "
                f"got {mbps:.1f} Mbps"
            )
        finally:
            _cleanup_tc(server)

    def test_ingress_rate_limit(self, wg_tunnel):
        """Apply 10 Mbps ingress limit (via IFB) and verify bandwidth is capped.

        Ingress on server = traffic FROM client = client upload.
        """
        server, client = wg_tunnel
        limit_mbit = 10

        try:
            _setup_tc_infrastructure(server)
            _add_peer_limit(server, CLIENT_VPN_IP, limit_mbit)

            # Measure upload (client→server): normal iperf3 direction
            mbps = _run_iperf3(client, SERVER_VPN_IP, reverse=False)
            print(f"\nIngress limited to {limit_mbit} Mbps, measured: {mbps:.1f} Mbps")

            max_expected = limit_mbit * 1.3
            assert mbps <= max_expected, (
                f"Ingress rate limit not working: expected <={max_expected:.0f} Mbps, "
                f"got {mbps:.1f} Mbps"
            )
        finally:
            _cleanup_tc(server)

    def test_rate_limit_update(self, wg_tunnel):
        """Change rate limit from 10 to 50 Mbps and verify new rate applies."""
        server, client = wg_tunnel
        initial_limit = 10
        updated_limit = 50

        try:
            _setup_tc_infrastructure(server)
            _add_peer_limit(server, CLIENT_VPN_IP, initial_limit)

            # Verify initial limit
            mbps = _run_iperf3(client, SERVER_VPN_IP, reverse=True)
            print(f"\nInitial limit {initial_limit} Mbps, measured: {mbps:.1f} Mbps")
            assert mbps <= initial_limit * 1.3

            # Update limit
            _update_peer_limit(server, CLIENT_VPN_IP, updated_limit)

            mbps = _run_iperf3(client, SERVER_VPN_IP, reverse=True)
            print(f"Updated limit {updated_limit} Mbps, measured: {mbps:.1f} Mbps")
            assert mbps <= updated_limit * 1.3
            # Should be noticeably higher than old limit
            assert mbps > initial_limit * 1.5, (
                f"Rate limit update not effective: {mbps:.1f} Mbps still near old limit"
            )
        finally:
            _cleanup_tc(server)

    def test_rate_limit_removal(self, wg_tunnel):
        """Remove rate limit and verify bandwidth returns to high levels."""
        server, client = wg_tunnel
        limit_mbit = 10

        try:
            _setup_tc_infrastructure(server)
            _add_peer_limit(server, CLIENT_VPN_IP, limit_mbit)

            # Verify limit is applied
            mbps_limited = _run_iperf3(client, SERVER_VPN_IP, reverse=True)
            print(f"\nWith limit: {mbps_limited:.1f} Mbps")
            assert mbps_limited <= limit_mbit * 1.3

            # Remove limit
            _remove_peer_limit(server, CLIENT_VPN_IP)

            mbps_unlimited = _run_iperf3(client, SERVER_VPN_IP, reverse=True)
            print(f"After removal: {mbps_unlimited:.1f} Mbps")
            # Should be significantly faster than the limit
            assert mbps_unlimited > limit_mbit * 2, (
                f"Rate limit removal not effective: {mbps_unlimited:.1f} Mbps still low"
            )
        finally:
            _cleanup_tc(server)
