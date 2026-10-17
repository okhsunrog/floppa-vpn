#!/usr/bin/env python3
"""Local three-namespace acceptance of the PostgreSQL-backed node (no WAN/host routes).

Requires sudo -n, iproute2, nft, PostgreSQL tools, openssl, ping and built binaries.
DATABASE_URL must identify an explicitly disposable, already migrated database.
This creates fixture rows in that database. Never point it at production.
Run with uv run --no-project tests/integration/test_foldfin_node.py --help.
"""

import argparse
import hashlib
import json
import os
import secrets
import shutil
import signal
import subprocess
import sys
import time
import urllib.parse
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def run(*args, **kwargs):
    return subprocess.run(
        args, check=True, text=True, capture_output=True, **kwargs
    ).stdout


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--node-binary", type=Path, required=True)
    parser.add_argument("--client-binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument(
        "--database-url",
        required=True,
        help="Disposable migrated DB; use a Unix socket so the node can connect from its namespace",
    )
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False, mode=0o700)
    # Local-only database connection is essential: this harness creates no host veth.
    query = urllib.parse.parse_qs(urllib.parse.urlsplit(args.database_url).query)
    if not query.get("host", [""])[0].startswith("/"):
        raise SystemExit("Use a local Unix-socket database URL (?host=/path/to/socket)")
    tag = f"ffn{os.getpid()}"
    client, server, exit_ns = (f"{tag}-{suffix}" for suffix in ("c", "s", "e"))
    namespaces = []
    processes = []
    files = []

    def ns(name, *command):
        return run("sudo", "-n", "ip", "netns", "exec", name, *command)

    def sql(statement):
        return run(
            "psql",
            args.database_url,
            "-X",
            "-qAt",
            "-v",
            "ON_ERROR_STOP=1",
            "-c",
            statement,
        ).strip()

    def launch(name, label, command, token=None):
        log = (output / f"{label}.log").open("w")
        files.append(log)
        env = os.environ.copy()
        if token is not None:
            env["FOLDFIN_TOKEN"] = token
        process = subprocess.Popen(
            [
                "sudo",
                "-n",
                "--preserve-env=FOLDFIN_TOKEN,TMPDIR",
                "ip",
                "netns",
                "exec",
                name,
                "setpriv",
                f"--reuid={os.getuid()}",
                f"--regid={os.getgid()}",
                "--init-groups",
                *command,
            ],
            env=env,
            stdout=log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        processes.append(process)
        return process

    def stop(process):
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGINT)
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
                raise RuntimeError("process failed joined shutdown")

    try:
        if (
            sql("SELECT COUNT(*) FROM users") != "0"
            or sql("SELECT COUNT(*) FROM foldfin_nodes") != "0"
        ):
            raise RuntimeError("database is not empty; use a fresh disposable database")
        for name in (client, server, exit_ns):
            run("sudo", "-n", "ip", "netns", "add", name)
            namespaces.append(name)
            ns(name, "ip", "link", "set", "lo", "up")
        # Both ends are created directly inside namespaces; no host routes or interfaces.
        ns(
            server,
            "ip",
            "link",
            "add",
            "outer",
            "type",
            "veth",
            "peer",
            "name",
            "outer",
            "netns",
            client,
        )
        ns(
            server,
            "ip",
            "link",
            "add",
            "egress",
            "type",
            "veth",
            "peer",
            "name",
            "egress",
            "netns",
            exit_ns,
        )
        for name, interface, address in [
            (server, "outer", "192.0.2.1/30"),
            (client, "outer", "192.0.2.2/30"),
            (server, "egress", "198.19.78.1/30"),
            (exit_ns, "egress", "198.19.78.2/30"),
        ]:
            ns(name, "ip", "addr", "add", address, "dev", interface)
            ns(name, "ip", "link", "set", interface, "up")
        for name, address in [(server, "fd42:78::1/126"), (exit_ns, "fd42:78::2/126")]:
            ns(name, "ip", "-6", "addr", "add", address, "dev", "egress", "nodad")
        for name, v4, v6 in [
            (server, "10.233.0.1/24", "fd33::1/120"),
            (client, "10.233.0.2/32", "fd33::2/128"),
        ]:
            ns(
                name,
                "ip",
                "tuntap",
                "add",
                "dev",
                "ff-tun",
                "mode",
                "tun",
                "user",
                str(os.getuid()),
            )
            ns(name, "ip", "addr", "add", v4, "dev", "ff-tun")
            ns(name, "ip", "-6", "addr", "add", v6, "dev", "ff-tun", "nodad")
            ns(name, "ip", "link", "set", "ff-tun", "mtu", "1280", "up")
        ns(
            server,
            "sysctl",
            "-qw",
            "net.ipv4.ip_forward=1",
            "net.ipv6.conf.all.forwarding=1",
        )
        ns(client, "ip", "route", "add", "198.19.78.0/30", "dev", "ff-tun")
        ns(client, "ip", "-6", "route", "add", "fd42:78::/126", "dev", "ff-tun")
        cert, key = output / "cert.pem", output / "key.pem"
        run(
            "openssl",
            "req",
            "-x509",
            "-newkey",
            "ec",
            "-pkeyopt",
            "ec_paramgen_curve:P-256",
            "-nodes",
            "-days",
            "1",
            "-keyout",
            str(key),
            "-out",
            str(cert),
            "-subj",
            "/CN=localhost",
            "-addext",
            "subjectAltName=DNS:localhost",
            "-addext",
            "basicConstraints=critical,CA:FALSE",
            "-addext",
            "extendedKeyUsage=serverAuth",
        )
        config = output / "config.toml"
        config.write_text(f"""region = "singapore"
listen = "0.0.0.0:8443"
interface = "ff-tun"
certificate = {json.dumps(str(cert))}
private_key = {json.dumps(str(key))}
metrics = "127.0.0.1:19104"
sync_interval_secs = 1
lease_secs = 4
[public]
endpoints = ["192.0.2.1:8443"]
server_name = "localhost"
ipv4_pool = "10.233.0.0/24"
ipv6_pool = "fd33::/120"
routes = ["198.19.78.0/30", "fd42:78::/126"]
allow_destinations = ["198.19.78.0/30", "fd42:78::/126"]
dns = ["198.19.78.2"]
""")
        secret_file = output / "secrets.toml"
        secret_file.write_text("database_url = " + json.dumps(args.database_url) + "\n")
        secret_file.chmod(0o600)
        node = [
            str(args.node_binary.resolve()),
            "--config",
            str(config),
            "--secrets",
            str(secret_file),
        ]
        run(*node, "register")
        rendered = run(*node, "network", "--uplink", "egress")
        firewall = output / "node.nft"
        firewall.write_text(
            rendered[
                rendered.index("table inet ") : rendered.index("# Existing firewall")
            ]
        )
        ns(server, "nft", "-f", str(firewall))
        user = sql("INSERT INTO users DEFAULT VALUES RETURNING id")
        plan = sql(
            f"INSERT INTO plans(name,display_name) VALUES('{tag}','Node acceptance') RETURNING id"
        )
        sql(
            f"INSERT INTO plan_regions VALUES({plan},'singapore'); INSERT INTO subscriptions(user_id,plan_id,starts_at,is_current) VALUES({user},{plan},NOW(),true)"
        )
        installation = sql(
            f"INSERT INTO app_installations(user_id,device_id,region_id) VALUES({user},'{tag}','singapore') RETURNING id"
        )
        session = sql(
            f"INSERT INTO sessions(id,user_id,installation_id,kind) VALUES(gen_random_uuid(),{user},{installation},'credential') RETURNING id"
        )
        token = secrets.token_hex(32)
        digest = hashlib.sha256(token.encode()).hexdigest()
        sql(
            f"INSERT INTO foldfin_devices(installation_id,region_id,slot,token_digest,issued_by_session) VALUES({installation},'singapore',2,decode('{digest}','hex'),'{session}')"
        )
        node_process = launch(server, "node", [*node, "serve"])
        for _ in range(50):
            if node_process.poll() is not None:
                raise RuntimeError("node startup failed; inspect node.log")
            if ":8443" in ns(server, "ss", "-ltn"):
                break
            time.sleep(0.1)
        else:
            raise RuntimeError("node listener did not start")
        echo = output / "echo.py"
        echo.write_text("""import selectors,socket
sel=selectors.DefaultSelector()
for family,address in [(socket.AF_INET,"198.19.78.2"),(socket.AF_INET6,"fd42:78::2")]:
 for kind in (socket.SOCK_STREAM,socket.SOCK_DGRAM):
  s=socket.socket(family,kind);s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1);s.bind((address,19009))
  if kind==socket.SOCK_STREAM:s.listen()
  sel.register(s,selectors.EVENT_READ,kind)
while True:
 for key,_ in sel.select():
  if key.data==socket.SOCK_DGRAM:
   data,peer=key.fileobj.recvfrom(65535);key.fileobj.sendto(data,peer)
  else:
   c,_=key.fileobj.accept()
   with c:
    while data:=c.recv(65535):c.sendall(data)
""")
        launch(
            exit_ns,
            "echo",
            [
                shutil.which("uv"),
                "run",
                "--no-project",
                "--no-config",
                "--no-cache",
                "--python",
                sys.executable,
                str(echo),
            ],
        )
        probe = output / "probe.py"
        probe.write_text("""import socket
for family,address in [(socket.AF_INET,"198.19.78.2"),(socket.AF_INET6,"fd42:78::2")]:
 for kind in (socket.SOCK_STREAM,socket.SOCK_DGRAM):
  with socket.socket(family,kind) as s:
   s.settimeout(3);s.connect((address,19009));data=b"foldfin"*128;s.sendall(data)
   got=b""
   while len(got)<len(data):got+=s.recv(65535)
   assert got==data
print("IPv4/IPv6 TCP and UDP echo passed")
""")
        # Parent environment is inherited without writing a token to argv or artifacts.
        for transport in ("h2", "h3"):
            process = launch(
                client,
                transport,
                [
                    str(args.client_binary.resolve()),
                    "client",
                    "--transport",
                    transport,
                    "--server",
                    "192.0.2.1:8443",
                    "--server-name",
                    "localhost",
                    "--ca",
                    str(cert),
                    "--tun",
                    "ff-tun",
                    "--vpn-local",
                    "10.233.0.2,fd33::2",
                    "--vpn-peer",
                    "10.233.0.1,fd33::1",
                    "--vpn-peer-routes",
                    "198.19.78.0/30,fd42:78::/126",
                ],
                token,
            )
            for attempt in range(50):
                if process.poll() is not None or node_process.poll() is not None:
                    raise RuntimeError(
                        "node/client exited before readiness; inspect logs"
                    )
                try:
                    ns(client, "ping", "-c", "1", "-W", "1", "198.19.78.2")
                    break
                except subprocess.CalledProcessError:
                    time.sleep(0.1)
            else:
                raise RuntimeError("IPv4 routed ping failed")
            ns(client, "ping", "-6", "-c", "2", "-W", "1", "fd42:78::2")
            result = ns(
                client,
                shutil.which("uv"),
                "run",
                "--no-project",
                "--no-config",
                "--no-cache",
                "--python",
                sys.executable,
                str(probe),
            )
            (output / f"{transport}-result.txt").write_text(
                result + "IPv4/IPv6 ICMP passed\n"
            )
            stop(process)
        stop(node_process)
        if node_process.returncode != 0:
            raise RuntimeError(f"node shutdown failed: {node_process.returncode}")
        print(
            f"PASS: unprivileged node, H2/H3, real TUN, IPv4/IPv6 TCP/UDP/ICMP; artifacts {output}"
        )
    finally:
        errors = []
        for process in reversed(processes):
            try:
                stop(process)
            except (OSError, subprocess.SubprocessError, RuntimeError) as error:
                errors.append(error)
        for name in reversed(namespaces):
            try:
                run("sudo", "-n", "ip", "netns", "delete", name)
            except (OSError, subprocess.SubprocessError, RuntimeError) as error:
                errors.append(error)
        for file in files:
            file.close()
        # Credentials belong only to this disposable fixture.
        for path in (output / "key.pem", output / "secrets.toml"):
            path.unlink(missing_ok=True)
        if errors:
            raise RuntimeError(f"cleanup failed: {errors}")


if __name__ == "__main__":
    main()
