#!/usr/bin/env python3
"""Run via sudo -n unshare --mount --net uv run --no-project python this-file.
All network, /etc and /var/lib mutations are confined to private namespaces.
"""
import os
from pathlib import Path
import subprocess

assert os.geteuid() == 0
for kind in ('net', 'mnt'):
    assert os.readlink(f'/proc/self/ns/{kind}') != os.readlink(f'/proc/1/ns/{kind}'), 'refusing host namespaces'

def run(*args, check=True):
    result = subprocess.run(args, text=True, capture_output=True)
    if check and result.returncode:
        raise AssertionError((args, result.returncode, result.stdout, result.stderr))
    return result

run('mount', '--make-rprivate', '/')
run('mount', '-t', 'sysfs', 'sysfs', '/sys')
for target in ('/var/lib', '/etc'):
    run('mount', '-t', 'tmpfs', 'tmpfs', target)
helper_path = Path(__file__).resolve().parents[2] / 'floppa-vpn-core/resources/linux/floppa-network-helper'
state = Path('/var/lib/floppa-vpn/network-rollback-v1')
resolver = Path('/etc/resolv.conf')

def helper(*args, check=True):
    return run('bash', str(helper_path), *args, check=check)

def ip(*args):
    return run('ip', *args).stdout

# A standalone owner leaves persistent changes behind without calling teardown. Another
# invocation (the service/new owner) restores root-captured state, without the old HOME.
original = 'nameserver 192.0.2.53\n# original resolver\n'
resolver.write_text(original)
mark = run('sysctl', '-n', 'net.ipv4.conf.all.src_valid_mark').stdout.strip()
helper('check-policy')
helper('ensure-tun', 'floppa0', '1000')
helper('configure', 'floppa0', '10.0.0.2/32')
helper('add-routes', 'floppa0', '0.0.0.0/1', '128.0.0.0/1')
helper('add-policy', '1', '0')
helper('set-dns-file', '1.1.1.1')
assert resolver.read_text() != original
assert (state / 'dns').stat().st_uid == 0
assert state.stat().st_mode & 0o777 == 0o700
assert run('setpriv', '--reuid', '65534', '--regid', '65534', '--clear-groups',
           'cat', str(state / 'dns'), check=False).returncode != 0
# A second capture must not replace the original backup before failing.
assert helper('set-dns-file', '9.9.9.9', check=False).returncode != 0
assert (state / 'resolver-backup').read_text() == original
helper('recover-network')
assert resolver.read_text() == original
assert not Path('/sys/class/net/floppa0').exists()
assert run('sysctl', '-n', 'net.ipv4.conf.all.src_valid_mark').stdout.strip() == mark
helper('check-policy')
helper('recover-network')  # Idempotent recovery.
helper('ensure-tun', 'floppa0', '65534')
assert Path('/sys/class/net/floppa0/owner').read_text().strip() == '65534'
helper('configure', 'floppa0', '10.0.0.2/32')
# A route failure after the first successful mutation retains enough data for recovery.
assert helper('add-routes', 'floppa0', '10.50.0.0/16', '999.0.0.0/8', check=False).returncode != 0
assert (state / 'routes').exists()
assert '10.50.0.0/16' in ip('-N', 'route', 'show', 'table', 'all')
helper('recover-network')
assert '10.50.0.0/16' not in ip('-N', 'route', 'show', 'table', 'all')

# Preserve a relative symlink, including its target's contents.
resolver.unlink()
target = Path('/etc/upstream-resolver')
target.write_text(original)
resolver.symlink_to('upstream-resolver')
helper('set-dns-file', '1.1.1.1', '2606:4700:4700::1111')
assert target.read_text() == original
helper('recover-dns')
assert resolver.is_symlink() and os.readlink(resolver) == 'upstream-resolver'
assert target.read_text() == original
resolver.unlink()
helper('set-dns-file', '1.1.1.1')
helper('recover-dns')
assert not resolver.exists()  # Originally absent, not a stale generated config.

# A journal write refusal prevents the first policy mutation.
(state / 'policy').mkdir()
assert helper('add-policy', '1', '0', check=False).returncode != 0
assert not run('nft', 'list', 'tables').stdout
assert '31000:' not in ip('rule', 'show')
(state / 'policy').rmdir()

# Corrupt evidence is never treated as an empty journal or deleted.
(state / 'policy').write_bytes(b'bad\0')
(state / 'policy').chmod(0o600)
assert helper('recover-network', check=False).returncode != 0
assert (state / 'policy').read_bytes() == b'bad\0'
(state / 'policy').unlink()

# Failed undo retains the snapshot, then succeeds when the obstruction is removed.
resolver.write_text(original)
helper('set-dns-file', '1.1.1.1')
resolver.unlink()
resolver.mkdir()
assert helper('recover-dns', check=False).returncode != 0
assert (state / 'dns').exists() and (state / 'resolver-backup').read_text() == original
resolver.rmdir()
helper('recover-dns')
assert resolver.read_text() == original

# Simulate records from a previous boot: kernel state is not replayed into the new boot.
helper('add-policy', '1', '0')
record = (state / 'policy').read_bytes().split(b'\0')
record[3] = b'00000000-0000-0000-0000-000000000000'
(state / 'policy').write_bytes(b'\0'.join(record))
helper('recover-policy')
assert '31000:' in ip('rule', 'show')  # Left alone; this is now new-boot state.
assert not (state / 'policy').exists()
print('PASS: shared root recovery, cross-UID TUN ownership, DNS file/link/absence, failed writes/undo, corrupt evidence, boot isolation')
