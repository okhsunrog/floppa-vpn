#!/usr/bin/env python3
"""Fresh Foldfin DNS on marked sockets, isolated from the host VPN. Singapore only."""
import argparse
import ipaddress
import hashlib
import json
from pathlib import Path
import secrets
import shlex
import shutil
import socket
import signal
import subprocess
import time

ROOT = Path(__file__).resolve().parents[2]


def run(*cmd, check=True, **kw):
    return subprocess.run(list(map(str, cmd)), check=check, text=True, capture_output=True, **kw)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--recovery-client', type=Path)
    parser.add_argument('--server-binary', type=Path)
    args = parser.parse_args()
    assert bool(args.recovery_client) == bool(args.server_binary), 'both recovery binaries are required'
    binary = args.binary.resolve()
    assert binary.is_file()
    args.output.mkdir(parents=True, exist_ok=False)
    args.output.chmod(0o700)
    suffix = secrets.token_hex(4)
    namespace, veth, tag = 'ffdns-'+suffix, 'ffd'+suffix, 'foldfin-dns-'+suffix
    table = str(43000 + secrets.randbelow(1000))
    network = ipaddress.ip_network(f'10.253.{1 + secrets.randbelow(250)}.0/30')
    gateway_ns, client_ip = str(network[1]), str(network[2])
    host = 'sg.okhsunrog.dev'
    address = socket.gethostbyname(host)  # Administration/marker only, not tested DNS.
    assert ipaddress.ip_address(address).is_global
    undo, processes, files = [], [], []
    remote_started = False
    summary = {'namespace': namespace, 'network': str(network), 'table': table,
               'server': host, 'server_ip': address, 'passed': False, 'cleanup': False}

    def root(*cmd, **kw): return run('sudo', '-n', *cmd, **kw)
    def ns(*cmd, **kw): return root('ip', 'netns', 'exec', namespace, *cmd, **kw)
    def ssh(*cmd, **kw):
        return run('ssh', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=10',
                   'root@'+host, shlex.join(map(str, cmd)), timeout=kw.pop('timeout', 20), **kw)
    def change(action, rollback):
        undo.append(rollback)
        action()
    def rule(family, chain, *items):
        change(lambda: root('iptables', '-w', '-t', family, '-I', chain, '1', *items),
               lambda: root('iptables', '-w', '-t', family, '-D', chain, *items))
    def launch(name, cmd):
        log = open(args.output/name, 'w')
        files.append(log)
        process = subprocess.Popen(list(map(str, cmd)), stdout=log, stderr=log, start_new_session=True)
        processes.append(process)
        return process
    def ready(name, predicate, process, budget=15):
        deadline = time.monotonic() + budget
        while time.monotonic() < deadline:
            text = (args.output/name).read_text()
            if predicate(text): return text
            if process.poll() is not None: raise RuntimeError(name+': '+text)
            time.sleep(.1)
        raise TimeoutError(name)
    def snapshot():
        return {label: root(*cmd).stdout for label, cmd in {
            'defaults': ['ip', '-j', 'route', 'show', 'table', 'main', 'default'],
            'rules': ['ip', '-j', 'rule'],
            'forwarding': ['sysctl', '-n', 'net.ipv4.ip_forward'],
            'tcp_wmem': ['sysctl', '-n', 'net.ipv4.tcp_wmem'],
            'rp_filter': ['sysctl', '-n', 'net.ipv4.conf.all.rp_filter', 'net.ipv4.conf.default.rp_filter'],
        }.items()}

    firewalld = shutil.which('firewall-cmd') and root('firewall-cmd', '--state', check=False).returncode == 0
    trusted_before = root('firewall-cmd', '--zone=trusted', '--list-interfaces').stdout if firewalld else None
    def interrupted(_signal, _frame): raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, interrupted)
    dns_before = hashlib.sha256(Path('/etc/resolv.conf').read_bytes()).hexdigest()
    with binary.open('rb') as file:
        summary['binary_sha256'] = hashlib.file_digest(file, 'sha256').hexdigest()
    before = snapshot()
    summary['before'] = before
    try:
        routes = json.loads(before['defaults'])
        route = min((r for r in routes if r.get('gateway') and
                     (Path('/sys/class/net')/r['dev']/'device').exists()),
                    key=lambda r: r.get('metric', 0))
        dev, gateway = route['dev'], route['gateway']
        summary['physical_interface'] = dev
        assert before['forwarding'].strip() == '1', 'forwarding must already be enabled'
        assert not root('ip', 'route', 'show', 'table', table, check=False).stdout.strip()
        for link in json.loads(root('ip', '-j', 'addr').stdout):
            for addr in link.get('addr_info', []):
                assert ipaddress.ip_address(addr['local']) not in network, 'namespace subnet overlaps a host address'
        assert not any(str(r.get('table')) == table for r in json.loads(before['rules']))
        change(lambda: root('ip', 'netns', 'add', namespace),
               lambda: root('ip', 'netns', 'del', namespace))
        change(lambda: root('ip', 'link', 'add', veth, 'type', 'veth', 'peer', 'name', 'outer0', 'netns', namespace),
               lambda: root('ip', 'link', 'del', veth))
        root('ip', 'addr', 'add', gateway_ns+'/30', 'dev', veth)
        root('ip', 'link', 'set', veth, 'up')
        if firewalld:
            # iptables ACCEPT cannot bypass a later firewalld nftables rejection.
            # Grant forwarding only to this disposable interface, never the physical NIC.
            change(lambda: root('firewall-cmd', '--zone=trusted', '--add-interface='+veth),
                   lambda: root('firewall-cmd', '--zone=trusted', '--remove-interface='+veth))
        ns('ip', 'addr', 'add', client_ip+'/30', 'dev', 'outer0')
        ns('ip', 'link', 'set', 'outer0', 'up')
        ns('ip', 'link', 'set', 'lo', 'up')
        ns('ip', 'route', 'add', 'default', 'via', gateway_ns)
        # Model a retained but unusable full tunnel. Only marked outer sockets escape.
        ns('ip', 'route', 'add', 'blackhole', 'default', 'table', '2468')
        ns('ip', 'rule', 'add', 'not', 'fwmark', '0x666c6f70', 'lookup', '2468', 'priority', '100')
        change(lambda: root('ip', 'route', 'add', 'default', 'via', gateway, 'dev', dev, 'table', table),
               lambda: root('ip', 'route', 'del', 'default', 'via', gateway, 'dev', dev, 'table', table))
        change(lambda: root('ip', 'rule', 'add', 'from', network, 'lookup', table, 'priority', '89'),
               lambda: root('ip', 'rule', 'del', 'from', network, 'lookup', table, 'priority', '89'))
        comment = ['-m', 'comment', '--comment', tag]
        rule('nat', 'POSTROUTING', '-s', network, '-o', dev, *comment, '-j', 'MASQUERADE')
        rule('filter', 'FORWARD', '-i', veth, '-o', dev, *comment, '-j', 'ACCEPT')
        rule('filter', 'FORWARD', '-i', dev, '-o', veth, '-m', 'conntrack',
             '--ctstate', 'RELATED,ESTABLISHED', *comment, '-j', 'ACCEPT')
        summary['unmarked_route'] = ns('ip', 'route', 'get', '8.8.8.8', check=False).stderr
        assert summary['unmarked_route'], 'unmarked external route must be blocked'
        summary['namespace_rp_filter'] = ns('sysctl', 'net.ipv4.conf.all.rp_filter', 'net.ipv4.conf.outer0.rp_filter', 'net.ipv4.conf.lo.rp_filter').stdout
        # The synthetic blackhole also rejects reverse-path validation. Disable it
        # only on this disposable namespace interface, never on the host.
        ns('sysctl', '-q', '-w', 'net.ipv4.conf.outer0.rp_filter=0')
        summary['marked_route'] = ns('ip', 'route', 'get', '8.8.8.8', 'mark', '0x666c6f70').stdout
        summary['forwarding_route'] = root('ip', 'route', 'get', address, 'from', client_ip, 'iif', veth).stdout
        assert 'dev '+dev+' ' in summary['forwarding_route']
        capture = launch('physical.log', ['sudo', '-n', 'tcpdump', '--immediate-mode', '-nn', '-l', '-i', dev,
                         f'((host 8.8.8.8 or host 1.1.1.1 or host {address}) and tcp and (tcp[tcpflags] & (tcp-syn|tcp-rst) != 0)) or (host {address} and udp port 18445)'])
        ready('physical.log', lambda s: 'listening on' in s, capture)
        inner = launch('namespace.log', ['sudo', '-n', 'ip', 'netns', 'exec', namespace,
                       'tcpdump', '--immediate-mode', '-nn', '-l', '-i', 'any', 'tcp'])
        ready('namespace.log', lambda s: 'listening on' in s, inner)
        remote_started = True
        remote = launch('singapore.log', ['ssh', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=10', 'root@'+host,
                        shlex.join(['systemd-run', '--quiet', '--pipe', '--wait', '--collect', '--unit='+tag,
                                    '--property=RuntimeMaxSec=600', '--', 'tcpdump', '--immediate-mode', '-nn', '-l', '-i', 'any',
                                    f'host {address} and ((tcp and (port 22 or port 18445) and (tcp[tcpflags] & (tcp-syn|tcp-rst) != 0)) or udp port 18445)'])])
        ready('singapore.log', lambda s: 'listening on' in s, remote)
        probe = launch('probe.log', ['sudo', '-n', 'ip', 'netns', 'exec', namespace, binary, host, address+':22'])
        text = ready('probe.log', lambda s: any(line.startswith('{') for line in s.splitlines()), probe, 20)
        result = json.loads(next(line for line in text.splitlines() if line.startswith('{')))
        summary['probe'] = result
        assert address in result['addresses'], 'fresh DNS did not return the configured Singapore address'
        port = result['marker_local'].rsplit(':', 1)[1]
        connections = ssh('ss', '-Hnt', 'state', 'established', '( sport = :22 )').stdout
        (args.output/'singapore-connections.log').write_text(connections)
        peers = [line.split()[-1] for line in connections.splitlines()
                 if line.split()[-1].endswith(':'+port)]
        assert len(peers) == 1, 'ambiguous or NAT-rebased source port; refusing to guess'
        source = peers[0].rsplit(':', 1)[0].strip('[]')
        assert ipaddress.ip_address(source).is_global
        summary['vps_observed_source'] = source
        ready('physical.log', lambda s: '.'+port+' > '+address+'.22' in s and
              ('8.8.8.8.443' in s or '1.1.1.1.443' in s), capture)
        ready('singapore.log', lambda s: source+'.'+port in s, remote)
        if args.recovery_client:
            from foldfin_remote_recovery import exercise
            summary['recovery'] = exercise(args, namespace, address, source, tag, root, ns, ssh)
            assert any('18445' in line for line in (args.output/'physical.log').read_text().splitlines())
            assert any('18445' in line and source in line for line in (args.output/'singapore.log').read_text().splitlines())
        summary['passed'] = True
    finally:
        errors = []
        if remote_started:
            try:
                ssh('systemctl', 'stop', tag, check=False)
                state = ssh('systemctl', 'is-active', tag, check=False).stdout.strip()
                if state not in ('inactive', 'failed', 'unknown'):
                    errors.append('remote capture still active: '+state)
            except Exception as error: errors.append('remote capture cleanup: '+str(error))
        for process in reversed(processes):
            if process.poll() is None:
                root('kill', '-TERM', '--', '-'+str(process.pid), check=False)
                try: process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    root('kill', '-KILL', '--', '-'+str(process.pid), check=False)
                    process.wait(timeout=5)
        for file in files: file.close()
        for rollback in reversed(undo):
            try: rollback()
            except Exception as error: errors.append(str(error))
        summary['after'] = snapshot()
        if firewalld and root('firewall-cmd', '--zone=trusted', '--list-interfaces').stdout != trusted_before:
            errors.append('firewalld trusted interfaces were not restored')
        if hashlib.sha256(Path('/etc/resolv.conf').read_bytes()).hexdigest() != dns_before:
            errors.append('host resolver changed')
        summary['host_resolver_unchanged'] = not any(e == 'host resolver changed' for e in errors)
        if tag in root('iptables-save').stdout:
            errors.append('tagged firewall rules remain')
        if namespace in root('ip', 'netns', 'list').stdout:
            errors.append('namespace remains')
        summary['cleanup_errors'] = errors
        summary['cleanup'] = not errors and summary['after'] == before
        (args.output/'summary.json').write_text(json.dumps(summary, indent=2))
        assert summary['cleanup'], 'cleanup or host-state verification failed'
    print('PASS: fresh scoped DNS, marked physical path, VPS source and cleanup; '+str(args.output))


if __name__ == '__main__':
    main()
