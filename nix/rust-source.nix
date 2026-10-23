{
  pkgs,
  craneLib,
  manifests,
  code,
}:
let
  # Cargo still needs every workspace member for metadata/lock validation. Stub
  # unused members instead of making client builds depend on server source code.
  skeleton = craneLib.mkDummySrc { src = manifests; };
in
pkgs.runCommand "source" { } ''
  mkdir -p "$out"
  cp -r ${skeleton}/. "$out/"
  chmod -R u+w "$out"
  cp -r ${code}/. "$out/"
''
