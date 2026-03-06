"""Integration tests using gotatun (Mullvad's Rust WireGuard implementation)."""

import re

import pytest

from conftest import docker_exec


class TestGotatun:
    """Test VPN connectivity through gotatun tunnel."""

    def test_interface_exists(self, gotatun_container):
        """Verify the TUN interface was created by floppa-cli."""
        result = docker_exec(gotatun_container, ["ip", "link", "show", "floppa-test0"])
        assert "floppa-test0" in result.stdout

    def test_routes_configured(self, gotatun_container):
        """Verify routes were set up through the tunnel interface."""
        result = docker_exec(gotatun_container, ["ip", "route", "show"])
        assert "floppa-test0" in result.stdout, (
            f"No routes via floppa-test0:\n{result.stdout}"
        )

    def test_ping_server(self, gotatun_container, server_ip):
        """Verify we can ping the VPN server through the gotatun tunnel."""
        result = docker_exec(
            gotatun_container,
            ["ping", "-c", "3", "-W", "5", server_ip],
            timeout=20,
        )
        assert "0% packet loss" in result.stdout, (
            f"Ping to {server_ip} had packet loss:\n{result.stdout}"
        )

    def test_dns_resolution(self, gotatun_container):
        """Verify DNS resolution works through the gotatun tunnel."""
        result = docker_exec(
            gotatun_container,
            ["dig", "@1.1.1.1", "google.com", "+short", "+timeout=5"],
            timeout=15,
        )
        ips = [
            line.strip()
            for line in result.stdout.strip().splitlines()
            if re.match(r"^\d+\.\d+\.\d+\.\d+$", line.strip())
        ]
        assert len(ips) > 0, f"DNS resolution failed, output:\n{result.stdout}"

    def test_internet_reachability(self, gotatun_container, expected_exit_ip):
        """Verify internet access through the gotatun tunnel."""
        result = docker_exec(
            gotatun_container,
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
    def test_latency(self, gotatun_container, server_ip):
        """Measure VPN tunnel latency via gotatun (informational)."""
        result = docker_exec(
            gotatun_container,
            ["ping", "-c", "10", "-W", "5", server_ip],
            timeout=60,
        )
        match = re.search(
            r"rtt min/avg/max/mdev = ([\d.]+)/([\d.]+)/([\d.]+)/([\d.]+)",
            result.stdout,
        )
        if match:
            min_ms, avg_ms, max_ms, mdev_ms = (
                float(match.group(i)) for i in range(1, 5)
            )
            print(
                f"\n  Latency (gotatun): "
                f"min={min_ms:.1f}ms avg={avg_ms:.1f}ms "
                f"max={max_ms:.1f}ms mdev={mdev_ms:.1f}ms"
            )
        else:
            print(f"\n  Could not parse latency from:\n{result.stdout}")
