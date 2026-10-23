{
  lib,
  craneLib,
  manifestSrc,
  args,
  gui ? false,
}:
let
  read = path: builtins.unsafeDiscardStringContext (builtins.readFile path);
  lockPath = if gui then "floppa-client/src-tauri/Cargo.lock" else "Cargo.lock";
  lockText = read "${manifestSrc}/${lockPath}";
  guiManifest =
    if gui then builtins.fromTOML (read "${manifestSrc}/floppa-client/src-tauri/Cargo.toml") else null;
  # Only the application root is normalized. Dependency versions and path crate
  # constraints retain their real values; they must invalidate compiled artifacts.
  dependencyLock = builtins.toFile "floppa-dependencies-Cargo.lock" (
    if gui then
      builtins.replaceStrings
        [ "name = \"floppa-client\"\nversion = \"${guiManifest.package.version}\"" ]
        [ "name = \"floppa-client\"\nversion = \"0.0.0\"" ]
        lockText
    else
      lockText
  );
  dummyLib = craneLib.overrideScope (
    _: previous: {
      cleanCargoToml =
        input:
        let
          manifest = previous.cleanCargoToml input;
        in
        if (manifest.package.name or "") == "floppa-client" then
          manifest
          // {
            package = manifest.package // {
              version = "0.0.0";
            };
          }
        else
          manifest;
    }
  );
  dummySrc = dummyLib.mkDummySrc {
    src = manifestSrc;
    extraDummyScript = lib.optionalString gui ''
      cp ${dependencyLock} "$out/floppa-client/src-tauri/Cargo.lock"
    '';
  };
in
craneLib.buildDepsOnly (
  (builtins.removeAttrs args [
    "src"
    "version"
  ])
  // {
    inherit dummySrc;
    version = "0";
  }
)
