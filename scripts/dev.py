#!/usr/bin/env python3
"""Supervise the real API/frontend and an isolated Compose PostgreSQL for local work."""

import argparse
import base64
import fcntl
import hashlib
import os
from pathlib import Path
import re
import secrets
import shutil
import signal
import socket
import subprocess
import sys
import time
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
INSTANCE = os.environ.get("FLOPPA_DEV_INSTANCE", "local")
if not re.fullmatch(r"[a-z0-9-]{1,30}", INSTANCE):
    sys.exit(
        "FLOPPA_DEV_INSTANCE must contain 1–30 lowercase letters, digits or hyphens"
    )
STATE = ROOT / ".dev" / INSTANCE
PROJECT = f"floppa-dev-{hashlib.sha256(str(ROOT).encode()).hexdigest()[:8]}-{INSTANCE}"
DB_PORT = int(os.environ.get("FLOPPA_DEV_DB_PORT", "54329"))
API_PORT = int(os.environ.get("FLOPPA_DEV_API_PORT", "3100"))
WEB_PORT = int(os.environ.get("FLOPPA_DEV_WEB_PORT", "5173"))
DATABASE_URL = (
    f"postgres://floppa_dev:local-development-only@127.0.0.1:{DB_PORT}/floppa_dev"
)
ENV = {
    **os.environ,
    "SQLX_OFFLINE": "true",
    "DATABASE_URL": DATABASE_URL,
    "FLOPPA_CONFIG": str(ROOT / "dev/config.toml"),
    "FLOPPA_SECRETS": str(STATE / "secrets.toml"),
    "FLOPPA_LISTEN": f"127.0.0.1:{API_PORT}",
    "FLOPPA_METRICS_LISTEN": "127.0.0.1:0",
    "FLOPPA_DEV_API": f"http://127.0.0.1:{API_PORT}",
    "FLOPPA_DEV_DB_PORT": str(DB_PORT),
    "FLOPPA_LOCAL_DEV": "1",
    "RUST_LOG": os.environ.get("RUST_LOG", "info"),
}
# Do not inherit a remote backend, configuration, or Cargo target path from the caller.
ENV.pop("CARGO_TARGET_DIR", None)
ENGINE = os.environ.get(
    "FLOPPA_DEV_CONTAINER", "podman" if shutil.which("podman") else "docker"
)
if ENGINE not in ("docker", "podman"):
    sys.exit("FLOPPA_DEV_CONTAINER must be docker or podman")
COMPOSE = [
    ENGINE,
    "compose",
    "--env-file",
    "/dev/null",
    "-p",
    PROJECT,
    "-f",
    str(ROOT / "dev/compose.yaml"),
]


def run(args, **kwargs):
    subprocess.run(args, cwd=ROOT, env=ENV, check=True, **kwargs)


def prepare():
    run(COMPOSE + ["up", "-d", "--wait"])
    keyfile = STATE / "secrets.toml"
    if not keyfile.exists():
        key = lambda: base64.b64encode(secrets.token_bytes(32)).decode()
        content = f'''# Generated for local development only; never uses host VPN keys.
database_url = "{DATABASE_URL}"
wg_private_key = "{key()}"
awg_private_key = "{key()}"
[auth]
jwt_secret = "{secrets.token_hex(32)}"
encryption_key = "{secrets.token_hex(32)}"
[vless]
reality_public_key = "{key()}"
reality_private_key = "{key()}"
'''
        with open(keyfile, "x", opener=lambda p, flags: os.open(p, flags, 0o600)) as f:
            f.write(content)
        (STATE / "db-port").write_text(str(DB_PORT))
    elif (STATE / "db-port").read_text() != str(DB_PORT):
        sys.exit(
            "Database port changed for this instance; choose a new FLOPPA_DEV_INSTANCE."
        )
    # memory-serve embeds the normal build; Vite still serves the live source during dev.
    run(["just", "build-frontend"])
    run(
        [
            "cargo",
            "build",
            "-p",
            "floppa-server",
            "--bin",
            "floppa-server",
            "--example",
            "seed_dev",
        ]
    )
    run([str(ROOT / "target/debug/floppa-server"), "--migrate"])
    run([str(ROOT / "target/debug/examples/seed_dev")])


def wait_http(url, child):
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        if child.poll() is not None:
            raise RuntimeError(f"Process exited before {url} became ready")
        try:
            with urllib.request.urlopen(url, timeout=1) as response:
                if response.status == 200:
                    return
        except (OSError, TimeoutError):
            time.sleep(0.2)
    raise RuntimeError(f"Timed out waiting for {url}")


def check_port(port):
    with socket.socket() as sock:
        # A previous test server may have closed connections still in TIME_WAIT.
        # Match the server's bind behavior while continuing to reject live listeners.
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        try:
            sock.bind(("127.0.0.1", port))
        except OSError as error:
            raise RuntimeError(
                f"Port {port} is occupied; set FLOPPA_DEV_API_PORT / FLOPPA_DEV_WEB_PORT"
            ) from error


def serve(api_only):
    check_port(API_PORT)
    if not api_only:
        check_port(WEB_PORT)
    prepare()
    children = []
    try:
        api = subprocess.Popen(
            [str(ROOT / "target/debug/floppa-server"), "--api-only"],
            cwd=ROOT,
            env=ENV,
            start_new_session=True,
        )
        children.append(api)
        wait_http(f"http://127.0.0.1:{API_PORT}/api/version", api)
        if not api_only:
            web = subprocess.Popen(
                [
                    "vp",
                    "preview" if os.environ.get("FLOPPA_DEV_PREVIEW") == "1" else "dev",
                    "--host",
                    "127.0.0.1",
                    "--port",
                    str(WEB_PORT),
                    "--strictPort",
                ],
                cwd=ROOT / "floppa-face",
                env=ENV,
                start_new_session=True,
            )
            children.append(web)
            wait_http(f"http://127.0.0.1:{WEB_PORT}/", web)
        print(
            f"\nReady: http://127.0.0.1:{API_PORT if api_only else WEB_PORT}/login",
            flush=True,
        )
        print("Accounts: dev-admin / dev-user. Password: floppa-local-2026", flush=True)
        print(
            "Ctrl+C stops API/frontend. PostgreSQL data persists; just dev-down stops the DB.",
            flush=True,
        )
        while all(child.poll() is None for child in children):
            time.sleep(0.5)
        raise RuntimeError("A dev process exited; stopping the other processes")
    finally:
        for child in reversed(children):
            if child.poll() is None:
                os.killpg(child.pid, signal.SIGTERM)
        for child in children:
            try:
                child.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(child.pid, signal.SIGKILL)
                child.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=["up", "api", "prepare", "down", "reset"])
    args = parser.parse_args()
    STATE.mkdir(parents=True, exist_ok=True, mode=0o700)
    with (STATE / "session.lock").open("w") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            sys.exit(
                "This dev instance is running. Stop it with Ctrl+C before resetting or stopping its database."
            )
        if args.command in ("up", "api"):
            serve(args.command == "api")
        elif args.command == "prepare":
            prepare()
        elif args.command == "down":
            run(COMPOSE + ["down"])
        else:
            # Only this checkout+instance's Compose volume; no arbitrary DATABASE_URL input.
            run(COMPOSE + ["down", "--volumes"])
            prepare()


def shutdown(*_):
    # Supervisors can signal both the process group and the wrapper. A second
    # signal must not interrupt the finally block while it joins child processes.
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    signal.signal(signal.SIGINT, signal.SIG_IGN)
    raise SystemExit(0)


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, shutdown)
    signal.signal(signal.SIGINT, shutdown)
    try:
        main()
    except KeyboardInterrupt:
        pass
    except (subprocess.CalledProcessError, RuntimeError, FileNotFoundError) as error:
        sys.exit(str(error))
