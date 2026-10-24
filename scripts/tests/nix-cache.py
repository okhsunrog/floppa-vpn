#!/usr/bin/env python3
"""Check Nix cache boundaries without compiling clients. Run with uv run --no-project.

Set TMPDIR to a disk-backed task directory on local machines.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

repo = Path(__file__).resolve().parents[2]
with tempfile.TemporaryDirectory(prefix='floppa-nix-cache-') as temporary:
    source = Path(temporary) / 'source'
    source.mkdir()
    files = subprocess.check_output(['git', 'ls-files', '-z'], cwd=repo).split(b'\0')
    for entry in files:
        if entry:
            relative = Path(os.fsdecode(entry))
            if (repo / relative).is_file() or (repo / relative).is_symlink():
                (source / relative).parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(repo / relative, source / relative, follow_symlinks=False)

    def evaluate():
        expression = '''let f = builtins.getFlake %s; in {
          cli = f.packages.x86_64-linux.cli.drvPath;
          cliDeps = f.packages.x86_64-linux.cli.cargoArtifacts.drvPath;
          cliRust = f.packages.x86_64-linux.cli.unwrapped.drvPath;
          gui = f.checks.x86_64-linux.gui.drvPath;
          guiDeps = f.checks.x86_64-linux.gui.cargoArtifacts.drvPath;
          guiRust = f.checks.x86_64-linux.gui.unwrapped.drvPath;
          bunDeps = f.checks.x86_64-linux.gui.frontend.bunDependencies.drvPath;
          helper = f.packages.x86_64-linux.cli.resources.drvPath;
        }''' % json.dumps(f'path:{source}')
        return json.loads(subprocess.check_output(
            ['nix', 'eval', '--json', '--impure', '--expr', expression], text=True))

    baseline = evaluate()
    server = source / 'floppa-daemon/src/main.rs'
    original = server.read_text()
    server.write_text(original + '\n// Cache boundary probe.\n')
    assert evaluate() == baseline, 'server code invalidates client builds'
    server.write_text(original)

    core = source / 'floppa-vpn-core/src/platform/linux.rs'
    original = core.read_text()
    core.write_text(original + '\n// Cache boundary probe.\n')
    changed = evaluate()
    for key in ('cliDeps', 'guiDeps', 'bunDeps'):
        assert changed[key] == baseline[key], (key, 'core edit invalidates dependencies')
    for key in ('cli', 'gui', 'cliRust', 'guiRust'):
        assert changed[key] != baseline[key], (key, 'core edit must rebuild client')
    core.write_text(original)

    for path, probe in [
        ('floppa-network-helper/src/main.rs', '\n// Helper source cache probe.\n'),
        ('floppa-vpn-core/resources/linux/dev.okhsunrog.floppa-vpn.policy', '\n<!-- Policy cache probe. -->\n'),
    ]:
        resource = source / path
        original = resource.read_text()
        resource.write_text(original + probe)
        changed = evaluate()
        for key in ('cliDeps', 'guiDeps', 'bunDeps', 'cliRust', 'guiRust'):
            assert changed[key] == baseline[key], (path, key, 'resource edit recompiles clients')
        for key in ('helper', 'cli', 'gui'):
            assert changed[key] != baseline[key], (path, key, 'resource edit must update assembly')
        resource.write_text(original)

    manifest = source / 'floppa-client/src-tauri/Cargo.toml'
    lock = source / 'floppa-client/src-tauri/Cargo.lock'
    import tomllib
    version = tomllib.loads(manifest.read_text())['package']['version']
    manifest.write_text(manifest.read_text().replace(f'version = "{version}"', 'version = "99.0.0"', 1))
    lock.write_text(lock.read_text().replace(
        f'name = "floppa-client"\nversion = "{version}"',
        'name = "floppa-client"\nversion = "99.0.0"'))
    changed = evaluate()
    for key in ('cliDeps', 'guiDeps', 'bunDeps'):
        assert changed[key] == baseline[key], (key, 'release version invalidates dependencies')
    assert changed['gui'] != baseline['gui'], 'application version must update the GUI'
    print('PASS: server isolation, helper/policy edits only rebuild assembly, stable Rust and Bun dependencies across releases')
