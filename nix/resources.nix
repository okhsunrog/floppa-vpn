{
  stdenvNoCC,
  lib,
  bash,
  coreutils,
  iproute2,
  nftables,
  util-linux,
  procps,
  systemd,
  src,
}:
let
  runtimePath = lib.makeBinPath [
    coreutils
    iproute2
    nftables
    util-linux
    procps
    systemd
  ];
in
stdenvNoCC.mkDerivation {
  pname = "floppa-network-resources";
  version =
    (builtins.fromTOML (builtins.readFile "${src}/floppa-client/src-tauri/Cargo.toml")).package.version;
  inherit src;
  dontBuild = true;
  installPhase = ''
    helper="$out/lib/floppa-vpn/floppa-network-helper"
    policy="$out/share/polkit-1/actions/dev.okhsunrog.floppa-vpn.policy"
    install -Dm755 floppa-vpn-core/resources/linux/floppa-network-helper "$helper"
    substituteInPlace "$helper" --replace-fail '#!/bin/bash' '#!${bash}/bin/bash'
    sed -i '2i export PATH="${runtimePath}"' "$helper"
    install -Dm644 floppa-vpn-core/resources/linux/dev.okhsunrog.floppa-vpn.policy "$policy"
    substituteInPlace "$policy" --replace-fail /usr/lib/floppa-vpn/floppa-network-helper "$helper"
  '';
}
