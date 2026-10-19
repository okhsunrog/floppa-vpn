{
  lib,
  pkgs,
  craneLib,
  pkg-config,
  openssl,
  makeBinaryWrapper,
  src,
  resourceSrc,
  version,
}:
let
  common = import ./common.nix {
    inherit
      lib
      pkgs
      resourceSrc
      version
      ;
  };
  args = {
    pname = "floppa";
    inherit src version;
    strictDeps = true;
    cargoExtraArgs = "--locked -p floppa";
    cargoVendorDir = import ./vendor.nix {
      inherit craneLib;
      cargoLock = "${src}/Cargo.lock";
    };
    nativeBuildInputs = [ pkg-config ];
    buildInputs = [ openssl ];
  };
  cargoArtifacts = craneLib.buildDepsOnly args;
in
craneLib.buildPackage (
  args
  // {
    inherit cargoArtifacts;
    nativeBuildInputs = args.nativeBuildInputs ++ [ makeBinaryWrapper ];
    inherit (common) postPatch;
    postInstall = ''
      mkdir -p $out/lib/systemd/system $out/lib/sysusers.d $out/lib/tmpfiles.d
      cp floppa/resources/systemd/* $out/lib/systemd/system/
      substituteInPlace $out/lib/systemd/system/*.service --replace-fail /usr/bin/floppa "$out/bin/floppa"
      cp floppa/resources/sysusers.d/* $out/lib/sysusers.d/
      cp floppa/resources/tmpfiles.d/* $out/lib/tmpfiles.d/
      wrapProgram $out/bin/floppa --prefix PATH : '${common.runtimePath}'
    '';
    passthru = {
      inherit cargoArtifacts;
      inherit (common) resources;
    };
    meta = {
      description = "Floppa VPN CLI and tunnel service";
      mainProgram = "floppa";
      license = lib.licenses.gpl3Plus;
      platforms = [ "x86_64-linux" ];
    };
  }
)
