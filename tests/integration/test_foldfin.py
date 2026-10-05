#!/usr/bin/env python3
"""Floppa actor + real TUN + Foldfin server in disposable namespaces. Run with uv run."""
import argparse
import contextlib
import hashlib
import json
import os
from pathlib import Path
import secrets
import signal
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]
FOLDFIN = ROOT.parent / 'rust/foldfin'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--no-build', action='store_true')
    parser.add_argument('--benchmark', action='store_true', help='release throughput without WAN in addition to acceptance')
    parser.add_argument('--chrome-tls', action='store_true')
    parser.add_argument('--seconds', type=int, default=10)
    parser.add_argument('--output', type=Path, default=Path('/tmp/floppa-foldfin-local-bench'))
    args = parser.parse_args()
    assert 5 <= args.seconds <= 60
    profile = 'release' if args.benchmark else 'debug'
    if not args.no_build:
        subprocess.run(['cargo', 'build', '-p', 'floppa'] + (['--release'] if args.benchmark else []) + (['--features','foldfin-chrome'] if args.chrome_tls else []), cwd=ROOT, check=True)
        subprocess.run(['cargo', 'build', '--bin', 'foldfin'] + (['--release'] if args.benchmark else []), cwd=FOLDFIN, check=True)
    def binary(root, name):
        metadata = json.loads(subprocess.check_output(['cargo', 'metadata', '--no-deps', '--format-version', '1'], cwd=root))
        return Path(metadata['target_directory'])/profile/name
    floppa, foldfin = binary(ROOT, 'floppa'), binary(FOLDFIN, 'foldfin')
    suffix = secrets.token_hex(4)
    client_ns, server_ns = f'foldfin-client-{suffix}', f'foldfin-server-{suffix}'
    created, processes, logs = [], [], []
    def root(*cmd, **kwargs):
        return subprocess.run(['sudo', '-n', *map(str, cmd)], check=True, capture_output=True, text=True, **kwargs)
    def ns(namespace, *cmd):
        return ['sudo', '-n', 'ip', 'netns', 'exec', namespace, *map(str, cmd)]
    def launch(namespace, logfile, *cmd):
        log = open(logfile, 'w'); logs.append(log)
        process = subprocess.Popen(ns(namespace, *cmd), stdout=log, stderr=log, start_new_session=True)
        processes.append(process); return process
    temporary = tempfile.TemporaryDirectory(prefix='floppa-foldfin-')
    try:
        with contextlib.nullcontext(temporary.name) as directory:
            temp = Path(directory)
            cert, key = temp/'cert.pem', temp/'key.pem'
            subprocess.run(['openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1', '-keyout', str(key), '-out', str(cert), '-subj', '/CN=localhost', '-addext', 'subjectAltName=DNS:localhost', '-addext', 'basicConstraints=critical,CA:FALSE'], check=True, capture_output=True)
            token = secrets.token_hex(32)
            config = temp/'foldfin.json'
            config.write_text(json.dumps({'protocol':'foldfin', 'config': {'version':1, 'endpoint':'192.0.2.2:8443', 'server_name':'localhost', 'token':token, 'address':'198.18.0.1/32', 'peer':'198.18.0.2', 'routes':['198.18.0.2/32','198.19.0.0/24'], 'dns':[], 'mtu':1280, 'chrome_tls':args.chrome_tls, 'additional_ca_pem':cert.read_text()}}))
            config.chmod(0o600)
            for namespace in [client_ns, server_ns]:
                root('ip', 'netns', 'add', namespace); created.append(namespace)
                root('ip', '-n', namespace, 'link', 'set', 'lo', 'up')
            root('ip', 'link', 'add', 'fc0', 'netns', client_ns, 'type', 'veth', 'peer', 'name', 'fs0', 'netns', server_ns)
            for namespace, dev, address in [(client_ns,'fc0','192.0.2.1/30'),(server_ns,'fs0','192.0.2.2/30')]:
                root('ip', '-n', namespace, 'addr', 'add', address, 'dev', dev)
                root('ip', '-n', namespace, 'link', 'set', dev, 'up')
            root('ip', '-n', client_ns, 'route', 'add', 'default', 'via', '192.0.2.2', 'dev', 'fc0')
            root('ip', 'netns', 'exec', server_ns, 'ip', 'tuntap', 'add', 'dev', 'fsvpn0', 'mode', 'tun')
            root('ip', '-n', server_ns, 'addr', 'add', '198.18.0.2/32', 'dev', 'fsvpn0')
            root('ip', '-n', server_ns, 'link', 'set', 'fsvpn0', 'up')
            root('ip', '-n', server_ns, 'route', 'add', '198.18.0.1/32', 'dev', 'fsvpn0')
            root('ip', '-n', server_ns, 'addr', 'add', '198.19.0.2/32', 'dev', 'lo')
            root('ip', 'netns', 'exec', server_ns, 'sysctl', '-q', '-w', 'net.ipv4.ip_forward=1')
            server = launch(server_ns, temp/'server.log', 'env', f'FOLDFIN_TOKEN={token}', foldfin, 'server', '--listen', '192.0.2.2:8443', '--cert', cert, '--key', key, '--tun', 'fsvpn0', '--vpn-local', '198.18.0.2', '--vpn-peer', '198.18.0.1', '--vpn-local-routes', '198.19.0.0/24')
            launch(server_ns, temp/'echo.log', os.sys.executable, FOLDFIN/'tests/vpn_netns.py', '--worker', 'echo', '--address', '198.19.0.2', '--expect-source', '198.18.0.1')
            time.sleep(.15)
            client = launch(client_ns, temp/'client.log', 'env', f'XDG_CONFIG_HOME={temp}/config', floppa, 'connect', '--config', config, '--no-dns')
            deadline = time.monotonic()+15
            while 'READY' not in (temp/'client.log').read_text():
                if client.poll() is not None or server.poll() is not None or time.monotonic()>deadline:
                    raise RuntimeError('Floppa actor failed to become ready')
                time.sleep(.05)
            if args.benchmark:
                args.output.mkdir(parents=True, exist_ok=False)
                iperf = launch(server_ns, temp/'iperf.log', 'iperf3','-s','-p','25201')
                time.sleep(.2)
                results=[]
                def proc_cpu(namespace, name):
                    pids=[pid for pid in root('ip','netns','pids',namespace).stdout.split() if root('cat',f'/proc/{pid}/comm').stdout.strip()==name]
                    assert len(pids)==1, (name,pids)
                    stat=root('cat',f'/proc/{pids[0]}/stat').stdout.rsplit(')',1)[1].split()
                    return (int(stat[11])+int(stat[12]))/os.sysconf('SC_CLK_TCK')
                for mode, address in [('direct','192.0.2.2'),('chrome' if args.chrome_tls else 'rustls','198.19.0.2')]:
                    for parallel in [1,4]:
                        for reverse in [False,True]:
                            name=f'{mode}-p{parallel}-'+('download' if reverse else 'upload')
                            before_client=proc_cpu(client_ns,'floppa'); before_server=proc_cpu(server_ns,'foldfin'); started=time.monotonic()
                            cmd=['iperf3','-c',address,'-p','25201','-t',str(args.seconds),'-O','2','-P',str(parallel),'-J','--get-server-output']
                            if reverse: cmd.append('-R')
                            measured=root('ip','netns','exec',client_ns,*cmd,timeout=args.seconds+20)
                            elapsed=time.monotonic()-started
                            data=json.loads(measured.stdout); assert 'error' not in data,data
                            (args.output/(name+'.json')).write_text(json.dumps(data,indent=2))
                            result={'mode':mode,'parallel':parallel,'direction':'download' if reverse else 'upload','mbps':data['end']['sum_received']['bits_per_second']/1e6,'client_cpu_percent':(proc_cpu(client_ns,'floppa')-before_client)/elapsed*100,'server_cpu_percent':(proc_cpu(server_ns,'foldfin')-before_server)/elapsed*100,'retransmits':data['end']['sum_sent'].get('retransmits')}
                            results.append(result)
                            (args.output/'summary.json').write_text(json.dumps({'client':'Floppa release','server':'Foldfin release','mtu':1280,'seconds':args.seconds,'client_sha256':hashlib.sha256(floppa.read_bytes()).hexdigest(),'server_sha256':hashlib.sha256(foldfin.read_bytes()).hexdigest(),'results':results},indent=2))
                            print(f"Local {name}: {result['mbps']:.1f} Mbps; client CPU {result['client_cpu_percent']:.1f}%, server CPU {result['server_cpu_percent']:.1f}%",flush=True)
                print('Benchmark artifacts: '+str(args.output),flush=True)
            mtu = json.loads(root('ip','-n',client_ns,'-j','link','show','floppa0').stdout)[0]['mtu']
            assert mtu == 1280, mtu
            for address in ['198.18.0.2','198.19.0.2']:
                root('ip','netns','exec',client_ns,'ping','-n','-c','2','-W','3',address)
            subprocess.run(ns(client_ns, os.sys.executable, FOLDFIN/'tests/vpn_netns.py', '--worker','probe','--address','198.19.0.2'), check=True, timeout=45)
            # Await actor rollback before namespace deletion, rather than relying on deletion.
            os.killpg(client.pid, signal.SIGINT); assert client.wait(timeout=15) == 0
            link=json.loads(root('ip','-n',client_ns,'-j','addr','show','floppa0').stdout)[0]
            assert not link['addr_info'], link
            routes=json.loads(root('ip','-n',client_ns,'-j','route','show').stdout)
            assert not any(route.get('dev') == 'floppa0' for route in routes), routes
            print('PASS Floppa Foldfin: real actor/TUN, MTU 1280, routed TCP/FIN/UDP/DNS/ICMP, awaited route/address rollback')
    except BaseException:
        for log in logs:
            try: print(Path(log.name).read_text())
            except OSError: pass
        raise
    finally:
        for namespace in reversed(created):
            pids=root('ip','netns','pids',namespace).stdout.split()
            if pids: root('kill','-KILL',*pids)
            root('ip','netns','del',namespace)
        for process in processes: process.wait(timeout=10)
        for log in logs: log.close()
        root('chown', '-R', f'{os.getuid()}:{os.getgid()}', temporary.name)
        temporary.cleanup()


if __name__ == '__main__':
    main()
