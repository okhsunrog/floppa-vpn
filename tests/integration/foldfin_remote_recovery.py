"""Temporary Singapore server and real Floppa actor, called inside the physical harness."""
import hashlib
import json
import os
from pathlib import Path
import secrets
import shlex
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
FOLDFIN = ROOT.parent / 'rust/foldfin'


def exercise(args, namespace, address, public_source, tag, root, ns, ssh):
    output = args.output / 'recovery'
    output.mkdir(mode=0o700)
    remote = '/opt/' + tag
    tun = 'ffr' + tag.rsplit('-', 1)[1]
    server_unit, echo_unit = tag+'-server', tag+'-echo'
    port = 18445
    local, peer = '198.18.79.1', '198.18.79.2'
    undo, processes = [], []
    result = {'transports': {}, 'cleanup': False}
    for label, path in [('client', args.recovery_client), ('server', args.server_binary)]:
        with path.open('rb') as file:
            result[label+'_sha256'] = hashlib.file_digest(file, 'sha256').hexdigest()
    original = ssh('systemctl', 'show', 'foldfin', '-p', 'ActiveState', '-p', 'MainPID').stdout
    def change(action, rollback):
        undo.append(rollback)
        return action()
    def upload(path, destination):
        with path.open('rb') as file:
            ssh('sh', '-c', 'umask 077; cat > '+shlex.quote(destination), stdin=file, timeout=120)
    def unit(name, *command):
        ssh('systemd-run', '--quiet', '--collect', '--unit='+name,
            '--property=RuntimeMaxSec=300', '--property=KillSignal=SIGINT',
            '--property=EnvironmentFile='+remote+'/token.env', '--', *command)
    def remote_rule(*items):
        change(lambda: ssh('iptables', '-w', '-I', 'INPUT', '1', *items),
               lambda: ssh('iptables', '-w', '-D', 'INPUT', *items))
    def states(path):
        return [json.loads(line) for line in path.read_text().splitlines() if line.startswith('{')]
    def wait(path, process, predicate, seconds=65):
        deadline = time.monotonic()+seconds
        while time.monotonic() < deadline:
            rows = states(path)
            found = next((row for row in reversed(rows) if predicate(row)), None)
            if found: return found
            if process.poll() is not None: raise RuntimeError('actor exited; inspect '+str(path))
            time.sleep(.15)
        raise TimeoutError('actor state; inspect '+str(path))
    def network():
        links = json.loads(ns('ip', '-j', 'addr', 'show', 'floppa77').stdout)
        return {'link': [{k: link[k] for k in ['ifindex', 'ifname', 'mtu', 'addr_info']} for link in links],
                'routes': json.loads(ns('ip', '-j', 'route', 'show', 'table', '1718382448').stdout),
                'rules': json.loads(ns('ip', '-j', 'rule').stdout)}
    def traffic(label):
        ping = ns('ping', '-n', '-c', '2', '-W', '5', local, timeout=15).stdout
        probe = ns(sys.executable, FOLDFIN/'tests/vpn_netns.py', '--worker', 'probe', '--address', local, timeout=60).stdout
        (output/(label+'-traffic.log')).write_text(ping+probe)
    try:
        assert not ssh('ip', 'route', 'show', local+'/32').stdout.strip()
        assert not ssh('ip', 'route', 'show', peer+'/32').stdout.strip()
        assert not ssh('ss', '-Hlntup', 'sport', '=', ':'+str(port)).stdout.strip()
        change(lambda: ssh('mkdir', '-m', '700', remote),
               lambda: ssh('rm', '-rf', '--', remote))
        upload(args.server_binary.resolve(), remote+'/foldfin')
        upload(FOLDFIN/'tests/vpn_netns.py', remote+'/echo.py')
        ssh('chmod', '700', remote+'/foldfin')
        token = secrets.token_hex(32)
        token_file = output/'token.env'
        token_file.write_text('FOLDFIN_TOKEN='+token+'\n')
        token_file.chmod(0o600)
        upload(token_file, remote+'/token.env')
        token_file.unlink()
        cert = ssh('cat', '/etc/foldfin/server.pem').stdout
        change(lambda: ssh('ip', 'tuntap', 'add', 'dev', tun, 'mode', 'tun'),
               lambda: ssh('ip', 'link', 'del', tun))
        ssh('ip', 'addr', 'add', local+'/32', 'dev', tun)
        ssh('ip', 'link', 'set', tun, 'up')
        change(lambda: ssh('ip', 'route', 'add', peer+'/32', 'dev', tun),
               lambda: ssh('ip', 'route', 'del', peer+'/32', 'dev', tun))
        comment = ['-m', 'comment', '--comment', tag]
        for proto in ['tcp', 'udp']:
            remote_rule('-s', public_source, '-p', proto, '--dport', str(port), *comment, '-j', 'ACCEPT')
        remote_rule('-i', tun, '-s', peer, *comment, '-j', 'ACCEPT')
        change(lambda: unit(server_unit, remote+'/foldfin', 'server', '--h3',
                            '--listen', '0.0.0.0:'+str(port), '--cert', '/etc/foldfin/server.pem',
                            '--key', '/etc/foldfin/server.key', '--tun', tun,
                            '--vpn-local', local, '--vpn-peer', peer),
               lambda: ssh('systemctl', 'stop', server_unit))
        change(lambda: unit(echo_unit, 'uv', 'run', '--no-project', remote+'/echo.py',
                            '--worker', 'echo', '--address', local, '--expect-source', peer),
               lambda: ssh('systemctl', 'stop', echo_unit))
        # Replace the earlier synthetic block with the actual actor-owned policy routing.
        ns('ip', 'rule', 'del', 'not', 'fwmark', '0x666c6f70', 'lookup', '2468', 'priority', '100')
        ns('ip', 'route', 'del', 'blackhole', 'default', 'table', '2468')
        for transport in ['h2_only', 'h3_only']:
            directory = output/transport
            directory.mkdir(mode=0o700)
            for name in ['owner', 'state']:
                root('install', '-d', '-o', 'root', '-g', 'root', '-m', '700', directory/name)
            config = directory/'config.json'
            config.write_text(json.dumps({'protocol':'foldfin', 'config': {
                'version':2, 'endpoints':[f'sg.okhsunrog.dev:{port}'],
                'server_name':'sg.okhsunrog.dev', 'token':token, 'transport':transport,
                'additional_ca_pem':cert, 'network': {'revision':1,
                    'addresses':[peer+'/32'], 'peers':[local], 'routes':[local+'/32'],
                    'dns':[local], 'mtu':1280}}}))
            config.chmod(0o600)
            log = directory/'actor.log'
            # No host ownership lock, rollback state or service IPC may be shared.
            command = ['sudo', '-n', 'ip', 'netns', 'exec', namespace,
                       'unshare', '--mount', '--propagation', 'private', 'sh', '-ec',
                       'mount --bind "$1" /run/floppa-vpn; mount --bind "$2" /var/lib/floppa-vpn; shift 2; exec "$@"',
                       'sh', directory/'owner', directory/'state', 'env',
                       'XDG_CONFIG_HOME='+str(directory/'config'), 'TMPDIR='+str(directory),
                       args.recovery_client.resolve(), config]
            with log.open('w') as file:
                process = subprocess.Popen(list(map(str, command)), stdout=file, stderr=file, start_new_session=True)
            processes.append(process)
            first = wait(log, process, lambda s: s['phase'] == 'connected')
            before = network()
            traffic(transport+'-before')
            # Kill only this namespace's established H2 socket. H3 is interrupted by
            # restarting the disposable server, which also exercises server state replacement.
            if transport == 'h2_only':
                killed = ns('ss', '-K', 'dst', address, 'dport', '=', ':'+str(port)).stdout
                (directory/'socket-kill.log').write_text(killed)
                assert killed.strip(), 'no H2 socket was destroyed'
            else:
                ssh('systemctl', 'restart', server_unit)
            recovered = wait(log, process, lambda s: s['phase'] == 'connected' and
                             s['outcome_serial'] > first['outcome_serial'], 90)
            assert any(s['phase'] == 'retrying' for s in states(log)), 'no actor recovery observed'
            after = network()
            assert before == after, 'TUN identity or platform network state changed'
            assert first['connected_at'] == recovered['connected_at']
            traffic(transport+'-after')
            root('kill', '-INT', '--', '-'+str(process.pid))
            assert process.wait(timeout=45) == 0
            routes = ns('ip', '-j', 'route', 'show', 'table', '1718382448', check=False).stdout
            assert not routes.strip() or json.loads(routes) == []
            assert 'STOPPED' in log.read_text()
            result['transports'][transport] = {'before':before, 'after':after,
                'first':first, 'recovered':recovered, 'traffic_before_after':True, 'rollback':True}
            config.unlink()
            print(transport+': remote actor recovery and retained TUN PASS', flush=True)
    finally:
        for process in processes:
            if process.poll() is None:
                root('kill', '-INT', '--', '-'+str(process.pid), check=False)
                try: process.wait(timeout=45)
                except subprocess.TimeoutExpired:
                    root('kill', '-KILL', '--', '-'+str(process.pid), check=False)
                    process.wait(timeout=5)
        for name in [server_unit, echo_unit]:
            try: (output/(name+'.log')).write_text(ssh('journalctl', '-u', name, '--no-pager', '-n', '100', check=False).stdout)
            except Exception: pass
        errors = []
        for rollback in reversed(undo):
            try: rollback()
            except Exception as error: errors.append(str(error))
        (output/'token.env').unlink(missing_ok=True)
        for config in output.glob('*/config.json'): config.unlink()
        for directory in output.iterdir():
            if directory.is_dir():
                root('chown', '-R', f'{os.getuid()}:{os.getgid()}', directory)
        result['production_service_unchanged'] = original == ssh('systemctl', 'show', 'foldfin', '-p', 'ActiveState', '-p', 'MainPID').stdout
        result['cleanup_errors'] = errors
        result['cleanup'] = not errors and result['production_service_unchanged']
        (output/'summary.json').write_text(json.dumps(result, indent=2))
        assert result['cleanup'], 'remote recovery cleanup failed'
    return result
