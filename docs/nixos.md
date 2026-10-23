# NixOS

The flake builds Linux clients natively from source. The CLI is the default
package; the GUI also needs the deployment's non-secret API URL at build time.
Credentials and saved sessions are runtime state, never flake inputs.

```nix
# flake.nix inputs
floppa-vpn.url = "github:okhsunrog/floppa-vpn";
floppa-vpn.inputs.nixpkgs.follows = "nixpkgs";

# modules passed to nixosSystem
floppa-vpn.nixosModules.default

# host configuration, with floppa-vpn passed in specialArgs
services.floppa-vpn = {
  enable = true;
  users = [ "your-user" ];
  # Optional: retain compiled dependencies across Nix garbage collection.
  keepBuildDependencies = true;
  guiPackage = floppa-vpn.lib.mkClient {
    inherit pkgs;
    apiUrl = "https://your-deployment.example/api";
  };
};
```

Commit `flake.lock` to pin the exact source and dependencies. `nix flake update
floppa-vpn` deliberately updates that pin. Build the CLI with `nix build .#cli`;
run the disposable service integration test with `nix build .#checks.x86_64-linux.service`.

The module installs socket-activated systemd service, polkit policy, DNS tools
and capability wrappers. Members of `users` may control the service and run
standalone. `floppa connect` and the GUI use the running service when available;
otherwise they use the existing standalone ownership lock and error reporting.
No additional Nix-specific tunnel owner or configuration format is introduced.

`/run/wrappers/bin/floppa` and `floppa-client` grant `CAP_NET_ADMIN` to members of
the `floppa` group. Socket marking remains mandatory. The store is immutable:
there is no `setcap` or privileged first-run installation into `/usr`.
The packaging contract uses compile-time `FLOPPA_PACKAGED_HELPER` and
`FLOPPA_PACKAGED_POLICY` paths. Both must be provided together. Packaged clients
validate the installed resources and report missing files instead of invoking
the portable installer. Root executes the store helper directly; unprivileged
standalone uses pkexec. Runtime environment variables cannot select privileged
executables. No Rust source substitution or embedded-resource rewriting is used.

Persistent service configuration, session and connection intent live in
`/var/lib/floppa-vpn`, owned by root (directory mode 0700). Client preferences
continue to use the existing user configuration directory. Autostart runs
`resume --if-enabled`: the saved Connect on boot preference remains authoritative;
set `services.floppa-vpn.autostart = false` to omit this boot unit altogether.

For local development, use the Git checkout (`nix build .#cli`), not an impure
`path:` input containing ignored `.env` files, credentials or build output.

Rust builds use Crane with separate dependency artifacts for the CLI workspace
and the standalone Tauri workspace. `Cargo.lock`, compiler/profile/features and
native build inputs determine those caches; ordinary application source edits
do not rebuild all external Rust dependencies. The first build is still a full
build, and final release linking (including GUI LTO) is not eliminated. Dependency
derivations use a stable version; the GUI application's version is normalized
only in its dummy dependency manifest and lock. Real application manifests and
versions remain intact in the final build.
GTK/GApps wrapping runs in a separate derivation after Rust compilation, so its
additional native inputs cannot invalidate GTK build-script fingerprints from
the dependency cache.

Rust, frontend and privileged helper inputs are filtered independently. Updating
web assets does not rebuild the CLI or its dependency cache. The GUI embeds the
frontend only in its final application build. Git dependencies are vendored from
their locked revisions; no additional aggregate Cargo hash is maintained.
Unused workspace members are represented by Crane stubs, retaining Cargo metadata
without depending on server implementation files. The Bun dependency output
remains checksum-pinned and depends only on workspace package manifests and lock.

The NixOS module builds the CLI and GUI against the caller's `pkgs`; standalone
consumers can also use `lib.mkCli { inherit pkgs; }`.

`nix build .#checks.x86_64-linux.gui` builds a verification GUI with a deliberately
non-working example API URL. Use `lib.mkClient` with the real deployment URL for
installation. Its dependency cache can also be built separately with
`nix build .#checks.x86_64-linux.gui.cargoArtifacts`.

Set `services.floppa-vpn.keepBuildDependencies = true` to retain both dependency
caches with the installed system generation. This increases its store closure;
it is useful on machines that compile updates locally. Alternatively, create
persistent out-links to the matching builds (use the same nixpkgs input as your system):

```sh
nix build .#cli.cargoArtifacts --out-link /path/to/persistent-cache/cli
nix build .#checks.x86_64-linux.gui.cargoArtifacts --out-link /path/to/persistent-cache/gui
```

These are GC roots, not directories to share with ordinary Cargo builds. Remove
the links when the caches are no longer useful. Keep the existing per-workspace
Cargo build directory configuration for local development.

The Nix workflow builds both packages, runs the NixOS service/DNS VM, and checks
cache boundaries with `uv run --no-project python scripts/tests/nix-cache.py`.
Its GitHub Actions store cache accelerates CI only. No external binary cache or
new signing/upload credentials are required. A public binary cache can be added
separately if downloading CI-built packages on client machines is desired;
nixpkgs, features and build configuration must match for substitution.

The DNS VM covers real resolved per-link apply/revert and preservation of another
link's updated resolver. File fallback tests preserve concurrent manager updates.
Physical Wi-Fi roaming and acceptance on other laptops remain device checks.
