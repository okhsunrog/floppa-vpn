{ lib, src }:
let
  select =
    paths:
    lib.cleanSourceWith {
      inherit src;
      name = "source";
      filter =
        path: type:
        let
          relative = lib.removePrefix "${src}/" path;
        in
        lib.any (
          selected:
          relative == selected
          || lib.hasPrefix "${selected}/" relative
          || (type == "directory" && lib.hasPrefix "${relative}/" selected)
        ) paths;
    };
  workspace = builtins.fromTOML (builtins.readFile "${src}/Cargo.toml");
in
{
  rust = select (
    workspace.workspace.members
    ++ [
      "Cargo.toml"
      "Cargo.lock"
      ".cargo"
      "tauri-plugin-vpn"
      "floppa-client/src-tauri"
    ]
  );
  frontend = select [
    "package.json"
    "bun.lock"
    "floppa-web-shared"
    "floppa-face"
    "floppa-client/package.json"
    "floppa-client/index.html"
    "floppa-client/tsconfig.app.json"
    "floppa-client/env.d.ts"
    "floppa-client/auto-imports.d.ts"
    "floppa-client/components.d.ts"
    "floppa-client/tsconfig.json"
    "floppa-client/tsconfig.node.json"
    "floppa-client/vite.config.ts"
    "floppa-client/src"
    "floppa-client/public"
    "floppa-client/scripts"
    "floppa-client/src-tauri/Cargo.toml"
    "floppa-client/src-tauri/tauri.conf.json5"
  ];
  resources = select [
    "floppa-vpn-core/resources/linux/floppa-network-helper"
    "floppa-vpn-core/resources/linux/dev.okhsunrog.floppa-vpn.policy"
  ];
}
