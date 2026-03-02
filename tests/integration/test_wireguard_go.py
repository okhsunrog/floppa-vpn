"""Integration tests using wireguard-go (standard Go userspace WireGuard)."""

import re

import pytest

from conftest import docker_exec


class TestWireGuardGo:
    """Test VPN connectivity through wireguard-go tunnel."""

    def test_handshake(self, wg_go_container):
        """Verify WireGuard handshake completed with the server."""
        result = docker_exec(wg_go_container, ["wg", "show", "floppa0", "dump"])
        lines = result.stdout.strip().splitlines()
        # First line is the interface, subsequent lines are peers
        assert len(lines) >= 2, "No peers found in wg show output"

        peer_line = lines[1]
        fields = peer_line.split("\t")
        assert len(fields) >= 5, f"Unexpected wg dump format: {peer_line}"

        last_handshake = int(fields[4])
        assert last_handshake != 0, "No handshake has occurred (last_handshake = 0)"

    def test_ping_server(self, wg_go_container, server_ip):
        """Verify we can ping the VPN server through the tunnel."""
        result = docker_exec(
            wg_go_container,
            ["ping", "-c", "3", "-W", "5", server_ip],
            timeout=20,
        )
        assert "0% packet loss" in result.stdout, (
            f"Ping to {server_ip} had packet loss:\n{result.stdout}"
        )

    def test_dns_resolution(self, wg_go_container):
        """Verify DNS resolution works through the tunnel."""
        result = docker_exec(
            wg_go_container,
            ["dig", "@1.1.1.1", "google.com", "+short", "+timeout=5"],
            timeout=15,
        )
        # Should return at least one IP address
        ips = [
            line.strip()
            for line in result.stdout.strip().splitlines()
            if re.match(r"^\d+\.\d+\.\d+\.\d+$", line.strip())
        ]
        assert len(ips) > 0, f"DNS resolution failed, output:\n{result.stdout}"

    def test_internet_reachability(self, wg_go_container, expected_exit_ip):
        """Verify internet access through the VPN tunnel."""
        result = docker_exec(
            wg_go_container,
            ["curl", "-4", "-s", "--max-time", "15", "https://ifconfig.me"],
            timeout=20,
        )
        ip = result.stdout.strip()
        assert re.match(r"^\d+\.\d+\.\d+\.\d+$", ip), (
            f"Expected an IP address, got: {ip}"
        )

        if expected_exit_ip:
            assert ip == expected_exit_ip, (
                f"Exit IP mismatch: got {ip}, expected {expected_exit_ip}"
            )

    @pytest.mark.slow
    def test_latency(self, wg_go_container, server_ip):
        """Measure VPN tunnel latency (informational, no hard failure threshold)."""
        result = docker_exec(
            wg_go_container,
            ["ping", "-c", "10", "-W", "5", server_ip],
            timeout=60,
        )
        # Parse rtt min/avg/max/mdev line
        match = re.search(
            r"rtt min/avg/max/mdev = ([\d.]+)/([\d.]+)/([\d.]+)/([\d.]+)",
            result.stdout,
        )
        if match:
            min_ms, avg_ms, max_ms, mdev_ms = (
                float(match.group(i)) for i in range(1, 5)
            )
            print(
                f"\n  Latency (wireguard-go): "
                f"min={min_ms:.1f}ms avg={avg_ms:.1f}ms "
                f"max={max_ms:.1f}ms mdev={mdev_ms:.1f}ms"
            )
        else:
            print(f"\n  Could not parse latency from:\n{result.stdout}")
