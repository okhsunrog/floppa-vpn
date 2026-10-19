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
Standalone uses the packaged polkit helper and identical embedded helper bytes.

Persistent service configuration, session and connection intent live in
`/var/lib/floppa-vpn`, owned by root (directory mode 0700). Client preferences
continue to use the existing user configuration directory. Autostart runs
`resume --if-enabled`: the saved Connect on boot preference remains authoritative;
set `services.floppa-vpn.autostart = false` to omit this boot unit altogether.

For local development, use the Git checkout (`nix build .#cli`), not an impure
`path:` input containing ignored `.env` files, credentials or build output.
