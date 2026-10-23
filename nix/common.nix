{
  lib,
  pkgs,
  resourceSrc,
  craneLib,
  version,
}:
let
  resources = pkgs.callPackage ./resources.nix {
    src = resourceSrc;
    inherit version craneLib;
  };
in
{
  inherit resources;
  env = {
    FLOPPA_PACKAGED_HELPER = "${resources}/lib/floppa-vpn/floppa-network-helper";
    FLOPPA_PACKAGED_POLICY = "${resources}/share/polkit-1/actions/dev.okhsunrog.floppa-vpn.policy";
  };
  # pkexec must resolve to NixOS's setuid wrapper, never polkit's bare store
  # executable. The same precedence retains the capability-wrapped clients.
  runtimePath =
    "/run/wrappers/bin:"
    + lib.makeBinPath (
      with pkgs;
      [
        iproute2
        nftables
        util-linux
        coreutils
        procps
        systemd
        xdg-utils
      ]
    );
}
