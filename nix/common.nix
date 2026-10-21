{
  lib,
  pkgs,
  resourceSrc,
  version,
}:
let
  resources = pkgs.callPackage ./resources.nix {
    src = resourceSrc;
    inherit version;
  };
in
{
  inherit resources;
  postPatch = ''
    substituteInPlace floppa-vpn-core/src/platform/linux.rs \
      --replace-fail '"/usr/lib/floppa-vpn/floppa-network-helper"' '"${resources}/lib/floppa-vpn/floppa-network-helper"' \
      --replace-fail '"/usr/share/polkit-1/actions/dev.okhsunrog.floppa-vpn.policy"' '"${resources}/share/polkit-1/actions/dev.okhsunrog.floppa-vpn.policy"'
    # Installed and embedded bytes must match, or standalone would try to run
    # an interactive installer against the immutable system at first connect.
    cp ${resources}/lib/floppa-vpn/floppa-network-helper floppa-vpn-core/resources/linux/
    cp ${resources}/share/polkit-1/actions/dev.okhsunrog.floppa-vpn.policy floppa-vpn-core/resources/linux/
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
