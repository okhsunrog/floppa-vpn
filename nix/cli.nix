{
  lib,
  pkgs,
  rustPlatform,
  pkg-config,
  openssl,
  makeBinaryWrapper,
  src,
}:
let
  common = import ./common.nix { inherit lib pkgs src; };
in
rustPlatform.buildRustPackage {
  pname = "floppa";
  version =
    (builtins.fromTOML (builtins.readFile "${src}/floppa-client/src-tauri/Cargo.toml")).package.version;
  inherit src;
  cargoHash = "sha256-rh4Z+MX7z7Kbew1IF6cVzZjiYMMFJgqatssz+87FamQ=";
  nativeBuildInputs = [
    pkg-config
    makeBinaryWrapper
  ];
  buildInputs = [ openssl ];
  inherit (common) postPatch;
  cargoBuildFlags = [
    "-p"
    "floppa"
  ];
  cargoTestFlags = [
    "-p"
    "floppa"
  ];
  postInstall = ''
    mkdir -p $out/lib/systemd/system $out/lib/sysusers.d $out/lib/tmpfiles.d
    cp floppa/resources/systemd/* $out/lib/systemd/system/
    substituteInPlace $out/lib/systemd/system/*.service --replace-fail /usr/bin/floppa "$out/bin/floppa"
    cp floppa/resources/sysusers.d/* $out/lib/sysusers.d/
    cp floppa/resources/tmpfiles.d/* $out/lib/tmpfiles.d/
    wrapProgram $out/bin/floppa --prefix PATH : '${common.runtimePath}'
  '';
  passthru.resources = common.resources;
  meta = {
    description = "Floppa VPN CLI and tunnel service";
    mainProgram = "floppa";
    license = lib.licenses.gpl3Plus;
    platforms = [ "x86_64-linux" ];
  };
}
