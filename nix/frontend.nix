{
  lib,
  stdenv,
  bun,
  nodejs_24,
  autoPatchelfHook,
  src,
  apiUrl,
}:
let
  modules = stdenv.mkDerivation {
    pname = "floppa-node-modules";
    version =
      (builtins.fromTOML (builtins.readFile "${src}/floppa-client/src-tauri/Cargo.toml")).package.version;
    inherit src;
    nativeBuildInputs = [ bun ];
    dontConfigure = true;
    dontFixup = true;
    buildPhase = ''
      export HOME="$TMPDIR"
      # Fixed-output dependency bootstrap: Vite+ is itself among the packages
      # being fetched and is not available until this step has completed.
      bun install --frozen-lockfile --ignore-scripts --no-progress
    '';
    installPhase = ''
      mkdir -p "$out"
      cp -a node_modules "$out/"
      for workspace in floppa-client floppa-face floppa-web-shared; do
        mkdir -p "$out/$workspace"
        cp -a "$workspace/node_modules" "$out/$workspace/"
      done
      rm -rf "$out/node_modules/.cache"
    '';
    outputHashMode = "recursive";
    outputHash = "sha256-HUGqdjQ5HHEMOV+STXpFbDZFWyk28di5GBRt8ISzEak=";
  };
in
stdenv.mkDerivation {
  pname = "floppa-frontend";
  version =
    (builtins.fromTOML (builtins.readFile "${src}/floppa-client/src-tauri/Cargo.toml")).package.version;
  inherit src;
  nativeBuildInputs = [
    nodejs_24
    bun
    autoPatchelfHook
  ];
  buildInputs = [ stdenv.cc.cc.lib ];
  # Bun's lock includes optional musl binaries as well as glibc binaries.
  # They are never selected on this glibc platform.
  autoPatchelfIgnoreMissingDeps = [ "libc.musl-x86_64.so.1" ];
  dontConfigure = true;
  buildPhase = ''
    export HOME="$TMPDIR"
    cp -a ${modules}/. .
    chmod -R u+w .
    patchShebangs node_modules
    autoPatchelf node_modules
    export PATH="$PWD/node_modules/.bin:$PATH"
    cd floppa-client
    vp run sync-version
    vp build
  '';
  env.VITE_API_URL = apiUrl;
  installPhase = ''cp -a dist "$out"'';
  passthru.nodeModules = modules;
}
