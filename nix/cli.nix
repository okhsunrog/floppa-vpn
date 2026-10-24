{
  lib,
  pkgs,
  craneLib,
  pkg-config,
  openssl,
  makeBinaryWrapper,
  src,
  manifestSrc,
  resourceSrc,
  version,
}:
let
  common = import ./common.nix {
    inherit
      lib
      pkgs
      resourceSrc
      craneLib
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
      cargoLock = "${manifestSrc}/Cargo.lock";
    };
    nativeBuildInputs = [ pkg-config ];
    buildInputs = [ openssl ];
  };
  cargoArtifacts = import ./dependencies.nix {
    inherit
      lib
      craneLib
      manifestSrc
      args
      ;
  };
  unwrapped = craneLib.buildPackage (
    args
    // {
      inherit cargoArtifacts;
      inherit (common) env;
      postInstall = ''
        mkdir -p $out/lib/systemd/system $out/lib/sysusers.d $out/lib/tmpfiles.d
        cp floppa/resources/systemd/* $out/lib/systemd/system/
        cp floppa/resources/sysusers.d/* $out/lib/sysusers.d/
        cp floppa/resources/tmpfiles.d/* $out/lib/tmpfiles.d/
      '';
      meta = {
        description = "Floppa VPN CLI and tunnel service";
        mainProgram = "floppa";
        license = lib.licenses.gpl3Plus;
        platforms = [ "x86_64-linux" ];
      };
    }
  );
in
pkgs.stdenvNoCC.mkDerivation {
  pname = "floppa";
  inherit version;
  dontUnpack = true;
  strictDeps = true;
  nativeBuildInputs = [ makeBinaryWrapper ];
  installPhase = ''
    runHook preInstall
    mkdir -p "$out"
    # Copy, not a symlink: current_exe must point into this assembled package.
    cp -r ${unwrapped}/. "$out/"
    chmod -R u+w "$out"
    # Retain the compiled output across GC, so a helper-only update only repacks.
    mkdir -p "$out/nix-support"
    ln -s ${unwrapped} "$out/nix-support/unwrapped"
    ${common.installResources}
    substituteInPlace $out/lib/systemd/system/*.service --replace-fail /usr/bin/floppa "$out/bin/floppa"
    wrapProgram $out/bin/floppa --prefix PATH : '${common.runtimePath}'
    runHook postInstall
  '';
  passthru = {
    inherit cargoArtifacts unwrapped;
    inherit (common) resources;
  };
  inherit (unwrapped) meta;
}
