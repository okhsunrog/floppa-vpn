{
  lib,
  pkgs,
  rustPlatform,
  pkg-config,
  openssl,
  gtk3,
  webkitgtk_4_1,
  libayatana-appindicator,
  wrapGAppsHook3,
  makeBinaryWrapper,
  src,
  apiUrl,
}:
let
  common = import ./common.nix { inherit lib pkgs src; };
  frontend = pkgs.callPackage ./frontend.nix { inherit src apiUrl; };
in
rustPlatform.buildRustPackage {
  pname = "floppa-client";
  version =
    (builtins.fromTOML (builtins.readFile "${src}/floppa-client/src-tauri/Cargo.toml")).package.version;
  inherit src;
  cargoRoot = "floppa-client/src-tauri";
  buildAndTestSubdir = "floppa-client/src-tauri";
  cargoHash = "sha256-5+yznvWGpV3G4ZxXBwEkmtEHQGo4adUNIrj4afU5VIk=";
  nativeBuildInputs = [
    pkg-config
    wrapGAppsHook3
    makeBinaryWrapper
  ];
  buildInputs = [
    openssl
    gtk3
    webkitgtk_4_1
    libayatana-appindicator
  ];
  postPatch = common.postPatch + ''
    cp -a ${frontend} floppa-client/dist
  '';
  cargoBuildFlags = [
    "--bin"
    "floppa-client"
  ];
  buildFeatures = [ "tauri/custom-protocol" ];
  # Tauri requires its production protocol when built without `tauri build`.
  env.TAURI_CONFIG = builtins.toJSON { build.frontendDist = "../dist"; };
  doCheck = false; # GUI/backend tests are run by the existing project CI.
  postInstall = ''
    install -Dm644 floppa-client/src-tauri/icons/128x128.png $out/share/icons/hicolor/128x128/apps/floppa-client.png
    install -Dm644 floppa-client/src-tauri/linux/floppa-client.desktop $out/share/applications/floppa-client.desktop
    sed -i '/NoDisplay=true/d' $out/share/applications/floppa-client.desktop
    gappsWrapperArgs+=(--prefix PATH : '${common.runtimePath}')
    # Tray support dlopens this library rather than linking it directly.
    gappsWrapperArgs+=(--prefix LD_LIBRARY_PATH : '${lib.makeLibraryPath [ libayatana-appindicator ]}')
  '';
  passthru = {
    inherit frontend;
    inherit (common) resources;
  };
  meta = {
    description = "Floppa VPN desktop client";
    mainProgram = "floppa-client";
    license = lib.licenses.gpl3Plus;
    platforms = [ "x86_64-linux" ];
  };
}
