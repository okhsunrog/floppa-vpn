#!/usr/bin/env python3
"""Rust helper recovery/migration tests; only private mount AND network namespaces.
Build with --features test-hooks, then run via sudo unshare --mount --net uv run.
The test binary itself also refuses host namespaces. No production fault switches.
"""
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import time

assert os.geteuid() == 0
for ns in ('net', 'mnt'):
    assert os.readlink(f'/proc/self/ns/{ns}') != os.readlink(f'/proc/1/ns/{ns}')
helper = Path(os.environ.get('FLOPPA_TEST_HELPER', 'target/debug/floppa-network-helper')).resolve()
real_nft = os.path.realpath(shutil.which('nft'))
real_ip = os.path.realpath(shutil.which('ip'))
bash = os.path.realpath(shutil.which('bash'))
base_path = os.pathsep.join(os.path.realpath(p) for p in os.environ['PATH'].split(os.pathsep))
env = os.environ.copy()
env['FLOPPA_TEST_PATH'] = base_path

def run(*args, check=True, environ=None, input=None):
    result = subprocess.run(args, text=True, capture_output=True, env=environ, input=input, timeout=20)
    assert not check or result.returncode == 0, (args, result.returncode, result.stdout, result.stderr)
    return result

def h(*args, check=True, crash=None):
    e = env.copy()
    if crash is not None:
        e['FLOPPA_TEST_CRASH'] = str(crash)
    return run(str(helper), *args, check=check, environ=e)

def ip(*args):
    return run(real_ip, *args).stdout

def nft(script):
    return run(real_nft, '-f', '-', input=script)

run('mount', '--make-rprivate', '/')
run('mount', '-t', 'sysfs', 'sysfs', '/sys')
for target in ('/etc', '/var/lib', '/run'):
    run('mount', '-t', 'tmpfs', 'tmpfs', target)
state = Path('/var/lib/floppa-vpn/network-rollback-v1')
resolver = Path('/etc/resolv.conf')
original = 'nameserver 192.0.2.53\n'
resolver.write_text(original)
boot = Path('/proc/sys/kernel/random/boot_id').read_text().strip()
mark_path = Path('/proc/sys/net/ipv4/conf/all/src_valid_mark')
mark = mark_path.read_text()
legacy = '''add table inet floppa_policy
add chain inet floppa_policy prerouting { type filter hook prerouting priority mangle; policy accept; }
add rule inet floppa_policy prerouting ct mark 0x666c6f70 meta mark set ct mark
add chain inet floppa_policy postrouting { type filter hook postrouting priority mangle; policy accept; }
add rule inet floppa_policy postrouting meta mark 0x666c6f70 ct mark set meta mark
'''

def clean():
    h('recover-network')
    h('recover-network')
    h('check-policy')
    assert resolver.read_text() == original
    assert not any((state / n).exists() for n in ('dns', 'policy', 'routes', 'link'))

# Exact reported upgrade failure: seven copies and an empty rollback journal.
h('check-policy')
for _ in range(7):
    nft(legacy)
h('recover-network')
h('check-policy')
assert mark_path.read_text() == mark  # No guessed sysctl without a snapshot.

# Only a complete, unmarked, known legacy layout can be adopted without a journal.
for extra in ('add rule inet floppa_policy prerouting accept\n',
              'add chain inet floppa_policy foreign\n'):
    nft(legacy + extra)
    before = run(real_nft, '-j', 'list', 'table', 'inet', 'floppa_policy').stdout
    assert h('recover-network', check=False).returncode != 0
    assert run(real_nft, '-j', 'list', 'table', 'inet', 'floppa_policy').stdout == before
    nft('delete table inet floppa_policy')
nft(legacy)
ip('rule', 'add', 'priority', '31010', 'lookup', 'main')
assert h('recover-network', check=False).returncode != 0
ip('rule', 'del', 'priority', '31010')
h('recover-network')

# Real old NUL-separated policy/routes/link records remain recoverable.
ip('tuntap', 'add', 'dev', 'floppa0', 'mode', 'tun', 'user', '1000')
ip('link', 'set', 'floppa0', 'up')
ip('route', 'add', '0.0.0.0/1', 'dev', 'floppa0', 'table', '1718382448')
for priority in ('31000', '31010'):
    args = ['rule', 'add', 'priority', priority, 'not', 'fwmark', '0x666c6f70', 'lookup', 'main' if priority == '31000' else '1718382448']
    if priority == '31000':
        args += ['suppress_prefixlength', '0']
    ip(*args)
nft(legacy)
for name, fields in [('policy', ['1', '0', mark.strip(), boot]),
                     ('routes', ['floppa0', boot]), ('link', ['floppa0', boot])]:
    (state / name).write_bytes(('\0'.join(fields) + '\0').encode())
    (state / name).chmod(0o600)
clean()
assert not Path('/sys/class/net/floppa0').exists()

# Crash after every write-ahead/command boundary. Restart recovery twice each time.
for args in [('ensure-tun', 'floppa0', '1000'), ('add-policy', '1', '1'),
             ('set-dns-file', '1.1.1.1'), ('add-routes', 'floppa0', '0.0.0.0/1', '::/1')]:
    reached_success = False
    for boundary in range(1, 24):
        if args[0] == 'add-routes':
            h('ensure-tun', 'floppa0', '1000')
            h('configure', 'floppa0', '10.0.0.2/32')
        result = h(*args, check=False, crash=boundary)
        assert result.returncode in (0, -signal.SIGKILL), (args, boundary, result.stderr)
        clean()
        assert mark_path.read_text() == mark
        assert not Path('/sys/class/net/floppa0').exists()
        if result.returncode == 0:
            reached_success = True
            break
    assert reached_success, args
    print('PASS: crash boundaries', args[0], flush=True)

# Recovery itself may be interrupted. Failed DNS undo must not strand routing cleanup.
for boundary in range(1, 16):
    h('ensure-tun', 'floppa0', '1000')
    h('configure', 'floppa0', '10.0.0.2/32')
    h('add-routes', 'floppa0', '0.0.0.0/1', '::/1')
    h('add-policy', '1', '1')
    h('set-dns-file', '1.1.1.1')
    result = h('recover-network', crash=boundary, check=False)
    assert result.returncode in (0, -signal.SIGKILL), result.stderr
    clean()
    if result.returncode == 0:
        break
else:
    raise AssertionError('recovery crash matrix did not finish')

h('ensure-tun', 'floppa0', '1000')
h('configure', 'floppa0', '10.0.0.2/32')
h('add-routes', 'floppa0', '0.0.0.0/1')
h('add-policy', '1', '0')
h('set-dns-file', '1.1.1.1')
resolver.rename('/etc/saved-installed')
resolver.mkdir()
assert h('recover-network', check=False).returncode != 0
assert (state / 'dns').exists() and (state / 'link').exists()
assert not (state / 'policy').exists() and not (state / 'routes').exists()
resolver.rmdir()
Path('/etc/saved-installed').rename(resolver)
clean()

# A TUN replaced under the same public name belongs to a different owner.
h('ensure-tun', 'floppa0', '1000')
ip('tuntap', 'del', 'dev', 'floppa0', 'mode', 'tun')
ip('tuntap', 'add', 'dev', 'floppa0', 'mode', 'tun', 'user', '1000')
assert h('recover-link', check=False).returncode != 0
assert (state / 'link').exists() and Path('/sys/class/net/floppa0').exists()
ip('tuntap', 'del', 'dev', 'floppa0', 'mode', 'tun')
clean()

# A foreign route added to the reserved table is preserved and remains actionable.
h('ensure-tun', 'floppa0', '1000')
h('configure', 'floppa0', '10.0.0.2/32')
h('add-routes', 'floppa0', '10.50.0.0/16')
ip('route', 'add', '10.60.0.0/16', 'dev', 'floppa0', 'table', '1718382448')
assert h('recover-routes', check=False).returncode != 0
assert (state / 'routes').exists()
assert '10.60.0.0/16' in ip('route', 'show', 'table', '1718382448')
ip('route', 'del', '10.60.0.0/16', 'dev', 'floppa0', 'table', '1718382448')
clean()

# Two simultaneous applies cannot append duplicate nft rules or replace undo data.
processes = [subprocess.Popen([str(helper), 'add-policy', '1', '0'], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE) for _ in range(2)]
for p in processes:
    p.communicate(timeout=20)
assert sorted(p.returncode for p in processes) == [0, 1]
table = json.loads(run(real_nft, '-j', 'list', 'table', 'inet', 'floppa_policy').stdout)
assert len([v for v in table['nftables'] if 'rule' in v]) == 2
clean()

# A foreign replacement under the same name must survive both journal and name collisions.
h('add-policy', '1', '0')
nft('delete table inet floppa_policy\nadd table inet floppa_policy')
assert h('recover-policy', check=False).returncode != 0
assert (state / 'policy').exists()
nft('delete table inet floppa_policy')
clean()

# Missing utilities / bad JSON are failures, never an empty ruleset. No grep dependency.
bin_dir = Path('/etc/test-bin')
bin_dir.mkdir()
for name, target in [('ip', real_ip), ('nft', real_nft)]:
    (bin_dir / name).symlink_to(target)
env['FLOPPA_TEST_PATH'] = str(bin_dir)
h('add-policy', '1', '0')
(bin_dir / 'nft').unlink()
saved = (state / 'policy').read_bytes()
assert h('recover-policy', check=False).returncode != 0
assert (state / 'policy').read_bytes() == saved
(bin_dir / 'nft').write_text(f'#!{bash}\nprintf "not-json\\n"\n')
(bin_dir / 'nft').chmod(0o755)
assert h('recover-policy', check=False).returncode != 0
assert (state / 'policy').read_bytes() == saved
(bin_dir / 'nft').unlink()
(bin_dir / 'nft').symlink_to(real_nft)
clean()

# SIGKILL of the parent must not release the journal lock while nft is still applying.
(bin_dir / 'nft').unlink()
(bin_dir / 'nft').write_text(f'''#!{bash}
if [[ "$1" == -f ]]; then
  : > /etc/child-ready
  while [[ ! -e /etc/child-release ]]; do sleep 0.02; done
fi
exec {real_nft} "$@"
''')
(bin_dir / 'nft').chmod(0o755)
env['FLOPPA_TEST_PATH'] = f'{bin_dir}:{base_path}'
p = subprocess.Popen([str(helper), 'add-policy', '1', '0'], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
try:
    deadline = time.monotonic() + 10
    while not Path('/etc/child-ready').exists():
        assert time.monotonic() < deadline
        time.sleep(.02)
    p.kill()
    p.wait(timeout=5)
    recovery = subprocess.Popen([str(helper), 'recover-network'], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    time.sleep(.15)
    assert recovery.poll() is None, 'recovery overtook orphaned mutation'
finally:
    Path('/etc/child-release').touch()
stdout, stderr = recovery.communicate(timeout=20)
assert recovery.returncode == 0, (stdout, stderr)
clean()

# Unsafe journal types, links, permissions, unknown versions remain untouched.
for kind in ('symlink', 'hardlink', 'fifo', 'permissions', 'version'):
    record = state / 'policy'
    if kind == 'symlink':
        record.symlink_to('/etc/missing')
    elif kind == 'hardlink':
        (state / 'record').write_text('{}')
        (state / 'record').chmod(0o600)
        os.link(state / 'record', record)
    elif kind == 'fifo':
        os.mkfifo(record, 0o600)
    else:
        record.write_text('{"version":999,"record":{}}')
        record.chmod(0o644 if kind == 'permissions' else 0o600)
    assert h('recover-policy', check=False).returncode != 0, kind
    assert record.exists() or record.is_symlink()
    record.unlink()
clean()
print('PASS: legacy upgrade, foreign ownership, crash/retry matrix, concurrent apply, child lock inheritance, missing tools, unsafe journals', flush=True)
