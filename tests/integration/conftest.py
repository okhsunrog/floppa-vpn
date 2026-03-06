"""Shared fixtures for VPN integration tests."""

import os
import re
import subprocess
import time
import uuid
from pathlib import Path

import pytest
from dotenv import load_dotenv

INTEGRATION_DIR = Path(__file__).resolve().parent
PROJECT_ROOT = INTEGRATION_DIR.parent.parent
DOCKER_IMAGE = "floppa-vpn-test"

load_dotenv(INTEGRATION_DIR / ".env")


def parse_wg_config(config_text: str) -> dict:
    """Parse a standard WireGuard .conf file into a dict."""
    config = {
        "interface": {},
        "peer": {},
    }
    section = None
    for line in config_text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        if line.lower() == "[interface]":
            section = "interface"
            continue
        if line.lower() == "[peer]":
            section = "peer"
            continue
        if "=" in line and section:
            key, _, value = line.partition("=")
            config[section][key.strip().lower()] = value.strip()
    return config


def docker_exec(
    container: str,
    cmd: list[str],
    timeout: int = 30,
    check: bool = True,
) -> subprocess.CompletedProcess:
    """Run a command inside a Docker container."""
    result = subprocess.run(
        ["docker", "exec", container] + cmd,
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    if check and result.returncode != 0:
        raise RuntimeError(
            f"docker exec {' '.join(cmd)} failed (rc={result.returncode}):\n"
            f"stdout: {result.stdout}\nstderr: {result.stderr}"
        )
    return result


def docker_exec_detach(container: str, cmd: list[str]) -> None:
    """Run a command inside a Docker container in detached mode."""
    subprocess.run(
        ["docker", "exec", "-d", container] + cmd,
        check=True,
    )


def docker_cp(src: str, container: str, dst: str) -> None:
    """Copy a file into a Docker container."""
    subprocess.run(["docker", "cp", src, f"{container}:{dst}"], check=True)


def _resolve_config_path() -> Path:
    """Resolve WG_TEST_CONFIG, treating relative paths as relative to tests/integration/."""
    config_path = os.environ.get("WG_TEST_CONFIG")
    if not config_path:
        pytest.skip("WG_TEST_CONFIG not set (create .env from .env.example)")
    path = Path(config_path)
    if not path.is_absolute():
        path = INTEGRATION_DIR / path
    return path.resolve()


@pytest.fixture(scope="session")
def wg_config() -> dict:
    """Load and parse WireGuard config from WG_TEST_CONFIG env var."""
    config_text = _resolve_config_path().read_text()
    return parse_wg_config(config_text)


@pytest.fixture(scope="session")
def wg_config_path() -> str:
    """Return the absolute path to the WireGuard config file."""
    return str(_resolve_config_path())


@pytest.fixture(scope="session")
def expected_exit_ip() -> str | None:
    """Optional expected exit IP for VPN tunnel verification."""
    return os.environ.get("WG_EXPECTED_EXIT_IP")


@pytest.fixture(scope="session")
def server_ip(wg_config) -> str:
    """Extract server VPN IP from config (address subnet .1)."""
    address = wg_config["interface"].get("address", "")
    # Address is like 10.100.0.5/32 — derive server IP as .1
    ip_part = address.split("/")[0]
    octets = ip_part.split(".")
    return f"{octets[0]}.{octets[1]}.{octets[2]}.1"


@pytest.fixture(scope="session")
def docker_image() -> str:
    """Build the Docker image for WireGuard testing."""
    dockerfile_dir = Path(__file__).parent
    subprocess.run(
        ["docker", "build", "-t", DOCKER_IMAGE, str(dockerfile_dir)],
        check=True,
        capture_output=True,
    )
    return DOCKER_IMAGE


@pytest.fixture(scope="session")
def tunnel_binary() -> str:
    """Return path to the pre-built floppa-cli binary."""
    binary = PROJECT_ROOT / "target" / "release" / "floppa-cli"
    if not binary.exists():
        pytest.skip(
            f"floppa-cli binary not found at {binary}. "
            "Build it first: cargo build --release -p floppa-cli"
        )
    return str(binary)


def _start_container(image: str, name: str) -> str:
    """Start a Docker container with NET_ADMIN and TUN device access."""
    subprocess.run(
        [
            "docker", "run", "-d",
            "--name", name,
            "--cap-add", "NET_ADMIN",
            "--device", "/dev/net/tun",
            image,
        ],
        check=True,
        capture_output=True,
    )
    return name


def _stop_container(name: str) -> None:
    """Stop and remove a Docker container."""
    subprocess.run(["docker", "rm", "-f", name], capture_output=True)


# --- wireguard-go fixtures ---


@pytest.fixture(scope="module")
def wg_go_container(docker_image, wg_config, wg_config_path, server_ip):
    """Start a container with wireguard-go tunnel configured."""
    name = f"floppa-wg-go-{uuid.uuid4().hex[:8]}"
    _start_container(docker_image, name)

    try:
        # Copy config file into container
        docker_cp(wg_config_path, name, "/test/wg0.conf")

        # Write a wg-compatible config (strip Interface section, keep only peer + private key)
        # wireguard-go needs a separate setconf file without Address/DNS
        iface = wg_config["interface"]
        peer = wg_config["peer"]

        wg_conf_lines = [f"[Interface]"]
        wg_conf_lines.append(f"PrivateKey = {iface['privatekey']}")
        if "listenport" in iface:
            wg_conf_lines.append(f"ListenPort = {iface['listenport']}")
        wg_conf_lines.append("")
        wg_conf_lines.append("[Peer]")
        wg_conf_lines.append(f"PublicKey = {peer['publickey']}")
        if "presharedkey" in peer:
            wg_conf_lines.append(f"PresharedKey = {peer['presharedkey']}")
        wg_conf_lines.append(f"Endpoint = {peer['endpoint']}")
        wg_conf_lines.append(f"AllowedIPs = {peer.get('allowedips', '0.0.0.0/0, ::/0')}")
        if "persistentkeepalive" in peer:
            wg_conf_lines.append(f"PersistentKeepalive = {peer['persistentkeepalive']}")

        wg_conf_content = "\n".join(wg_conf_lines) + "\n"

        # Write the wg-only config via docker exec
        docker_exec(name, ["sh", "-c", f"cat > /test/wg-only.conf << 'WGEOF'\n{wg_conf_content}WGEOF"])

        # Start wireguard-go
        docker_exec(name, ["wireguard-go", "floppa0"])
        time.sleep(0.5)

        # Apply WG config
        docker_exec(name, ["wg", "setconf", "floppa0", "/test/wg-only.conf"])

        # Configure IP address
        address = iface.get("address", "")
        docker_exec(name, ["ip", "addr", "add", address, "dev", "floppa0"])
        docker_exec(name, ["ip", "link", "set", "floppa0", "up"])

        # Add host route for WG endpoint via default gateway to prevent routing loop.
        # Without this, the catch-all routes (0.0.0.0/1, 128.0.0.0/1) would capture
        # the WG endpoint UDP traffic itself, creating a loop.
        endpoint = peer["endpoint"]
        endpoint_host = endpoint.rsplit(":", 1)[0]
        result = docker_exec(name, ["getent", "hosts", endpoint_host])
        endpoint_ip = result.stdout.strip().split()[0]

        result = docker_exec(name, ["ip", "route", "show", "default"])
        gateway_match = re.search(r"default via (\S+)", result.stdout)
        if gateway_match:
            gateway = gateway_match.group(1)
            docker_exec(name, ["ip", "route", "add", f"{endpoint_ip}/32", "via", gateway])

        # Add routes
        allowed_ips = peer.get("allowedips", "0.0.0.0/0, ::/0")
        for route in allowed_ips.split(","):
            route = route.strip()
            if route == "0.0.0.0/0":
                docker_exec(name, ["ip", "route", "add", "0.0.0.0/1", "dev", "floppa0"])
                docker_exec(name, ["ip", "route", "add", "128.0.0.0/1", "dev", "floppa0"])
            elif route == "::/0":
                docker_exec(name, ["ip", "route", "add", "::/1", "dev", "floppa0"], check=False)
                docker_exec(name, ["ip", "route", "add", "8000::/1", "dev", "floppa0"], check=False)
            else:
                docker_exec(name, ["ip", "route", "add", route, "dev", "floppa0"], check=False)

        # Wait for handshake
        _wait_for_handshake(name, "floppa0")

        yield name
    finally:
        _stop_container(name)


# --- gotatun fixtures ---


@pytest.fixture(scope="module")
def gotatun_container(docker_image, wg_config_path, tunnel_binary, server_ip):
    """Start a container with gotatun tunnel via floppa-cli."""
    name = f"floppa-gotatun-{uuid.uuid4().hex[:8]}"
    _start_container(docker_image, name)

    try:
        # Copy binary and config into container
        docker_cp(tunnel_binary, name, "/test/floppa-cli")
        docker_exec(name, ["chmod", "+x", "/test/floppa-cli"])
        docker_cp(wg_config_path, name, "/test/wg0.conf")

        # Start the tunnel binary in the background
        docker_exec_detach(name, [
            "/test/floppa-cli", "connect",
            "--config", "/test/wg0.conf",
            "--interface", "floppa-test0",
            "--no-dns",
        ])

        # Wait for the tunnel interface to come up
        deadline = time.time() + 15
        ready = False
        while time.time() < deadline:
            iface_check = docker_exec(name, ["ip", "link", "show", "floppa-test0"], check=False)
            if iface_check.returncode == 0:
                # Interface exists, wait a moment for routes to be configured
                time.sleep(2)
                ready = True
                break
            time.sleep(0.5)

        if not ready:
            logs = docker_exec(name, ["sh", "-c", "ps aux"], check=False)
            raise RuntimeError(
                f"gotatun tunnel did not come up within 15s.\nProcesses: {logs.stdout}"
            )

        yield name
    finally:
        _stop_container(name)


def generate_wg_keypair(container: str) -> tuple[str, str]:
    """Generate a WireGuard keypair inside a container. Returns (private_key, public_key)."""
    result = docker_exec(container, ["sh", "-c", "wg genkey"])
    private_key = result.stdout.strip()
    result = docker_exec(container, ["sh", "-c", f"echo '{private_key}' | wg pubkey"])
    public_key = result.stdout.strip()
    return private_key, public_key


def docker_network_create(name: str) -> str:
    """Create a Docker bridge network."""
    subprocess.run(
        ["docker", "network", "create", name],
        check=True,
        capture_output=True,
    )
    return name


def docker_network_remove(name: str) -> None:
    """Remove a Docker network."""
    subprocess.run(["docker", "network", "rm", name], capture_output=True)


def _start_container_on_network(image: str, name: str, network: str) -> str:
    """Start a Docker container with NET_ADMIN and TUN device on a specific network."""
    subprocess.run(
        [
            "docker", "run", "-d",
            "--name", name,
            "--network", network,
            "--cap-add", "NET_ADMIN",
            "--device", "/dev/net/tun",
            image,
        ],
        check=True,
        capture_output=True,
    )
    return name


def get_container_ip(container: str, network: str) -> str:
    """Get the container's IP address on a given Docker network."""
    result = subprocess.run(
        ["docker", "inspect", "-f",
         f'{{{{(index .NetworkSettings.Networks "{network}").IPAddress}}}}', container],
        capture_output=True, text=True, check=True,
    )
    return result.stdout.strip()


def _wait_for_handshake(container: str, iface: str, timeout: int = 15) -> None:
    """Wait for a WireGuard handshake to succeed."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        result = docker_exec(container, ["wg", "show", iface, "dump"], check=False)
        if result.returncode == 0:
            for line in result.stdout.strip().splitlines()[1:]:  # skip header
                fields = line.split("\t")
                if len(fields) >= 5 and fields[4] != "0":
                    return  # handshake timestamp is non-zero
        time.sleep(1)
    # Don't fail here — let individual tests check handshake
