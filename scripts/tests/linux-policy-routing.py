#!/usr/bin/env python3
"""Isolated integration test. Run: sudo -n unshare --net uv run --no-project python scripts/tests/linux-policy-routing.py
Requires iproute2, nftables and Linux network namespaces. Never runs in the host netns.
"""
import ipaddress
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import threading

assert os.geteuid() == 0, 'run through sudo -n unshare --net'
assert os.readlink('/proc/self/ns/net') != os.readlink('/proc/1/ns/net'), 'refusing host network namespace'
HELPER = Path(__file__).resolve().parents[2] / 'floppa-vpn-core/resources/linux/floppa-network-helper'
MARK = 0x666C6F70
TABLE = str(MARK)

def run(*args, check=True):
    return subprocess.run(args, check=check, text=True, capture_output=True).stdout.strip()

def ip(*args):
    return run('ip', *args)

def helper(*args):
    return run('bash', str(HELPER), *args)

def route(destination, dev, marked=False):
    args = ['-j', 'route', 'get', destination]
    if marked:
        args += ['mark', hex(MARK)]
    result = json.loads(ip(*args))[0]
    assert result['dev'] == dev, result

# A peer namespace holds a real echo server behind the underlay default route.
ready_read, ready_write = os.pipe()
peer = os.fork()
if peer == 0:
    os.close(ready_write)
    os.unshare(os.CLONE_NEWNET)
    os.read(ready_read, 1)
    def serve(family, kind, address):
        with socket.socket(family, kind) as s:
            s.bind((address, 5555))
            if kind == socket.SOCK_STREAM:
                s.listen()
                while True:
                    conn, _ = s.accept()
                    with conn:
                        conn.sendall(conn.recv(100))
            else:
                while True:
                    data, addr = s.recvfrom(100)
                    s.sendto(data, addr)
    for family, address in [(socket.AF_INET, '203.0.113.77'), (socket.AF_INET6, '2001:db8:99::77')]:
        for kind in [socket.SOCK_STREAM, socket.SOCK_DGRAM]:
            threading.Thread(target=serve, args=(family, kind, address), daemon=True).start()
    threading.Event().wait()
    os._exit(0)

os.close(ready_read)
try:
    # Wait until the child has actually unshared; no fixed timing assumption.
    import time
    deadline = time.monotonic() + 5
    while os.readlink(f'/proc/{peer}/ns/net') == os.readlink('/proc/self/ns/net'):
        assert time.monotonic() < deadline
        time.sleep(.01)
    ip('link', 'set', 'lo', 'up')
    ip('link', 'add', 'eth0', 'type', 'veth', 'peer', 'name', 'peer0')
    ip('link', 'set', 'peer0', 'netns', str(peer))
    def peer_ip(*args):
        return run('nsenter', '-t', str(peer), '-n', 'ip', *args)
    for dev in ['floppa0', 'corp0', 'usb0']:
        ip('link', 'add', dev, 'type', 'dummy')
        ip('link', 'set', dev, 'up')
    for addr in ['192.0.2.2/24', '2001:db8:1::2/64']:
        ip('addr', 'add', addr, 'dev', 'eth0', 'nodad')
    ip('link', 'set', 'eth0', 'up')
    peer_ip('link', 'set', 'lo', 'up')
    peer_ip('link', 'set', 'peer0', 'up')
    for addr in ['192.0.2.1/24', '2001:db8:1::1/64']:
        peer_ip('addr', 'add', addr, 'dev', 'peer0', 'nodad')
    for addr in ['203.0.113.77/32', '2001:db8:99::77/128']:
        peer_ip('addr', 'add', addr, 'dev', 'lo', 'nodad')
    ip('route', 'add', 'default', 'via', '192.0.2.1', 'metric', '100')
    ip('-6', 'route', 'add', 'default', 'via', '2001:db8:1::1', 'metric', '100')
    ip('route', 'add', '10.42.0.0/16', 'dev', 'corp0')
    ip('-6', 'route', 'add', '2001:db8:42::/48', 'dev', 'corp0')
    original = run('sysctl', '-n', 'net.ipv4.conf.all.src_valid_mark')
    run('sysctl', '-qw', 'net.ipv4.conf.all.rp_filter=1')
    # Same CIDR subtraction contract as configured excluded routes in the core.
    routes = [str(n) for n in ipaddress.ip_network('0.0.0.0/0').address_exclude(ipaddress.ip_network('194.186.63.0/24'))]
    routes += ['::/1', '8000::/1']
    helper('check-policy')
    helper('add-routes', 'floppa0', *routes)
    helper('add-policy', '1', '1')
    collision = subprocess.run(['bash', str(HELPER), 'check-policy'], capture_output=True)
    assert collision.returncode != 0 and b'already in use' in collision.stderr
    for destination in ['203.0.113.77', '2001:db8:99::77']:
        route(destination, 'floppa0')
        route(destination, 'eth0', True)
    route('194.186.63.5', 'eth0')
    route('10.42.0.5', 'corp0')
    route('2001:db8:42::5', 'corp0')
    route('192.0.2.1', 'eth0')
    os.write(ready_write, b'1')
    # Real TCP and UDP return traffic must pass strict rp_filter with conntrack marks.
    for family, address in [(socket.AF_INET, '203.0.113.77'), (socket.AF_INET6, '2001:db8:99::77')]:
        for kind in [socket.SOCK_STREAM, socket.SOCK_DGRAM]:
            deadline = time.monotonic() + 5
            while True:
                try:
                    with socket.socket(family, kind) as s:
                        s.setsockopt(socket.SOL_SOCKET, socket.SO_MARK, MARK)
                        s.settimeout(.5)
                        s.connect((address, 5555))
                        s.sendall(b'marked transport')
                        assert s.recv(100) == b'marked transport'
                    break
                except (ConnectionRefusedError, TimeoutError):
                    assert time.monotonic() < deadline, (family, kind)
                    time.sleep(.02)
    # Linux selects a new lowest-metric underlay without any endpoint host route rewrite.
    ip('route', 'add', 'default', 'dev', 'usb0', 'metric', '50')
    route('203.0.113.77', 'usb0', True)
    route('194.186.63.5', 'usb0')
    route('203.0.113.77', 'floppa0')
    ip('link', 'del', 'usb0')
    route('203.0.113.77', 'eth0', True)
    helper('del-policy', '1', '1', original)
    helper('del-routes', 'floppa0', *routes)
    # Cleanup is idempotent, including interrupted installation recovery.
    helper('del-policy', '1', '1', original)
    assert run('sysctl', '-n', 'net.ipv4.conf.all.src_valid_mark') == original
    assert '31010:' not in ip('rule', 'show')
    assert '31000:' not in ip('-6', 'rule', 'show')
    assert not run('nft', 'list', 'tables')
    route('203.0.113.77', 'eth0')
    route('10.42.0.5', 'corp0')
    helper('check-policy')
    # A partially applied policy must also be reversible.
    ip('rule', 'add', 'priority', '31000', 'not', 'fwmark', hex(MARK), 'lookup', 'main', 'suppress_prefixlength', '0')
    helper('del-policy', '1', '1', original)
    helper('check-policy')
    print('PASS: IPv4/IPv6 routing, exclusions, corporate routes, TCP/UDP strict rp_filter, underlay switch and cleanup')
finally:
    os.close(ready_write)
    os.kill(peer, signal.SIGTERM)
    os.waitpid(peer, 0)
