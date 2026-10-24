{ lib, src }:
let
  selectExcept =
    excluded: paths:
    lib.cleanSourceWith {
      inherit src;
      name = "source";
      filter =
        path: type:
        let
          relative = lib.removePrefix "${src}/" path;
        in
        !(lib.any (entry: relative == entry || lib.hasPrefix "${entry}/" relative) excluded)
        && lib.any (
          selected:
          relative == selected
          || lib.hasPrefix "${selected}/" relative
          || (type == "directory" && lib.hasPrefix "${relative}/" selected)
        ) paths;
    };
  select = selectExcept [ ];
  clientCode = selectExcept [ "floppa-vpn-core/resources/linux" ];
  workspace = builtins.fromTOML (builtins.readFile "${src}/Cargo.toml");
  shared = [
    "floppa-api-client"
    "floppa-core"
    "floppa-foldfin"
    "floppa-net"
    "floppa-provision"
    "floppa-tunnel-config"
    "floppa-vpn-core"
  ];
  manifests = [
    "Cargo.toml"
    "Cargo.lock"
    ".cargo"
  ]
  ++ map (member: "${member}/Cargo.toml") workspace.workspace.members;
in
{
  cliManifests = select (
    workspace.workspace.members
    ++ [
      "Cargo.toml"
      "Cargo.lock"
      ".cargo"
    ]
  );
  guiManifests = select (
    workspace.workspace.members
    ++ [
      "Cargo.toml"
      "Cargo.lock"
      ".cargo"
      "tauri-plugin-vpn"
      "floppa-client/src-tauri"
    ]
  );
  cliCode = clientCode (manifests ++ shared ++ [ "floppa" ]);
  guiCode = clientCode (
    manifests
    ++ shared
    ++ [
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
  resources = select (
    manifests
    ++ [
      "floppa-network-helper"
      "floppa-vpn-core/resources/linux/dev.okhsunrog.floppa-vpn.policy"
    ]
  );
}
