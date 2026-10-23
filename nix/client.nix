{
  lib,
  pkgs,
  craneLib,
  pkg-config,
  openssl,
  gtk3,
  webkitgtk_4_1,
  libayatana-appindicator,
  wrapGAppsHook3,
  makeBinaryWrapper,
  src,
  manifestSrc,
  resourceSrc,
  frontendSrc,
  bun2nix,
  version,
  apiUrl,
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
  frontend = pkgs.callPackage ./frontend.nix {
    src = frontendSrc;
    inherit apiUrl;
    inherit bun2nix;
  };
  lock = builtins.toFile "floppa-gui-Cargo.lock" (
    builtins.unsafeDiscardStringContext (
      builtins.readFile "${manifestSrc}/floppa-client/src-tauri/Cargo.lock"
    )
  );
  args = {
    pname = "floppa-client";
    inherit src version;
    strictDeps = true;
    cargoExtraArgs = "--locked --manifest-path floppa-client/src-tauri/Cargo.toml --bin floppa-client --features tauri/custom-protocol";
    # Cargo's nested workspace and Crane must use the same artifact directory.
    env.CARGO_TARGET_DIR = "target";
    cargoVendorDir = import ./vendor.nix {
      inherit craneLib;
      cargoLock = lock;
    };
    nativeBuildInputs = [ pkg-config ];
    buildInputs = [
      openssl
      gtk3
      webkitgtk_4_1
      libayatana-appindicator
    ];
    doCheck = false;
  };
  cargoArtifacts = import ./dependencies.nix {
    inherit
      lib
      craneLib
      manifestSrc
      args
      ;
    gui = true;
  };
  unwrapped = craneLib.buildPackage (
    args
    // {
      inherit cargoArtifacts;
      postPatch = "cp -a ${frontend} floppa-client/dist";
      env =
        args.env
        // common.env
        // {
          TAURI_CONFIG = builtins.toJSON { build.frontendDist = "../dist"; };
        };
      # Crane's automatic installer runs metadata in the root workspace, whose
      # lock differs from this standalone GUI workspace's lock.
      doNotPostBuildInstallCargoBinaries = true;
      installPhaseCommand = "install -Dm755 target/release/floppa-client $out/bin/floppa-client";
      postInstall = ''
        install -Dm644 floppa-client/src-tauri/icons/128x128.png $out/share/icons/hicolor/128x128/apps/floppa-client.png
        install -Dm644 floppa-client/src-tauri/linux/floppa-client.desktop $out/share/applications/floppa-client.desktop
        sed -i '/NoDisplay=true/d' $out/share/applications/floppa-client.desktop
      '';
      meta = {
        description = "Floppa VPN desktop client";
        mainProgram = "floppa-client";
        license = lib.licenses.gpl3Plus;
        platforms = [ "x86_64-linux" ];
      };
    }
  );
in
pkgs.stdenvNoCC.mkDerivation {
  pname = "floppa-client";
  inherit version;
  dontUnpack = true;
  strictDeps = true;
  nativeBuildInputs = [
    wrapGAppsHook3
    makeBinaryWrapper
  ];
  inherit (args) buildInputs;
  installPhase = ''
    runHook preInstall
    mkdir -p "$out"
    cp -r ${unwrapped}/. "$out/"
    chmod -R u+w "$out"
    gappsWrapperArgs+=(--prefix PATH : '${common.runtimePath}')
    # Tray support dlopens this library rather than linking it directly.
    gappsWrapperArgs+=(--prefix LD_LIBRARY_PATH : '${lib.makeLibraryPath [ libayatana-appindicator ]}')
    runHook postInstall
  '';
  passthru = {
    inherit frontend cargoArtifacts unwrapped;
    inherit (common) resources;
  };
  inherit (unwrapped) meta;
}
