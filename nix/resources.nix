{
  lib,
  craneLib,
  iproute2,
  nftables,
  systemd,
  src,
  version,
}:
let
  args = {
    pname = "floppa-network-resources";
    inherit src version;
    strictDeps = true;
    cargoExtraArgs = "--locked -p floppa-network-helper";
    cargoVendorDir = import ./vendor.nix {
      inherit craneLib;
      cargoLock = "${src}/Cargo.lock";
    };
    env.FLOPPA_HELPER_PATH = lib.makeBinPath [
      iproute2
      nftables
      systemd
    ];
  };
  cargoArtifacts = craneLib.buildDepsOnly (args // { version = "0"; });
in
craneLib.buildPackage (
  args
  // {
    inherit cargoArtifacts;
    passthru = { inherit cargoArtifacts; };
    postInstall = ''
      helper="$out/lib/floppa-vpn/floppa-network-helper-v2"
      policy="$out/share/polkit-1/actions/dev.okhsunrog.floppa-vpn.network-v2.policy"
      install -Dm755 "$out/bin/floppa-network-helper" "$helper"
      rm "$out/bin/floppa-network-helper"
      rmdir "$out/bin"
      install -Dm644 floppa-vpn-core/resources/linux/dev.okhsunrog.floppa-vpn.policy "$policy"
      substituteInPlace "$policy" --replace-fail /usr/lib/floppa-vpn/floppa-network-helper-v2 "$helper"
    '';
  }
)
