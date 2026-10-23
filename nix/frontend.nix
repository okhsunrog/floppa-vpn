{
  lib,
  stdenv,
  bun,
  cacert,
  nodejs_24,
  autoPatchelfHook,
  src,
  bun2nix,
  apiUrl,
}:
let
  dependencies = bun2nix.fetchBunDeps {
    # Workspaces are supplied by src, not downloaded dependencies. Excluding
    # them keeps their source edits out of the external dependency cache.
    bunNix =
      {
        fetchurl,
        fetchgit,
        fetchFromGitHub,
        ...
      }:
      import ./bun.nix {
        inherit fetchurl fetchgit fetchFromGitHub;
        copyPathToStore = _: null;
      };
    patchShebangs = false;
  };
in
stdenv.mkDerivation {
  pname = "floppa-frontend";
  version =
    (builtins.fromTOML (builtins.readFile "${src}/floppa-client/src-tauri/Cargo.toml")).package.version;
  inherit src;
  nativeBuildInputs = [
    nodejs_24
    bun2nix.hook
    bun
    autoPatchelfHook
  ];
  buildInputs = [ stdenv.cc.cc.lib ];
  # Bun's lock includes optional musl binaries as well as glibc binaries.
  # They are never selected on this glibc platform.
  autoPatchelfIgnoreMissingDeps = [ "libc.musl-x86_64.so.1" ];
  env.SSL_CERT_FILE = "${cacert}/etc/ssl/certs/ca-bundle.crt";
  bunDeps = dependencies;
  bunInstallFlags = [
    "--frozen-lockfile"
    "--linker=isolated"
    "--backend=copyfile"
  ];
  dontRunLifecycleScripts = true;
  dontUseBunPatch = true;
  dontUseBunBuild = true;
  dontUseBunCheck = true;
  dontUseBunInstall = true;
  dontConfigure = true;
  buildPhase = ''
    export HOME="$TMPDIR"
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
  passthru.bunDependencies = dependencies;
}
