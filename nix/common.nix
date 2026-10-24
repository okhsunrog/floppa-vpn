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
    # Stable compile-time layout, independent of the helper's store path. The
    # final package supplies root-owned symlinks next to the copied executable.
    FLOPPA_PACKAGED_LAYOUT = "relative-v2";
  };
  installResources = ''
    mkdir -p "$out/lib/floppa-vpn"
    ln -s ${resources}/lib/floppa-vpn/floppa-network-helper-v2 "$out/lib/floppa-vpn/floppa-network-helper-v2"
    ln -s ${resources}/share/polkit-1/actions/dev.okhsunrog.floppa-vpn.network-v2.policy "$out/lib/floppa-vpn/network-v2.policy"
  '';
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
