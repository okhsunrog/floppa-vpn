{
  description = "Floppa VPN Linux packages and NixOS integration";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/fe13242281a67641c0102cdb1087f0ebe586c7df";
  inputs.crane.url = "github:ipetkov/crane";
  outputs =
    {
      self,
      nixpkgs,
      crane,
    }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs { inherit system; };
      sources = import ./nix/sources.nix {
        inherit (nixpkgs) lib;
        src = self;
      };
      version =
        (builtins.fromTOML (builtins.readFile "${self}/floppa-client/src-tauri/Cargo.toml"))
        .package.version;
      mkPackages = pkgs: {
        craneLib = crane.mkLib pkgs;
        src = sources.rust;
        resourceSrc = sources.resources;
        inherit version;
      };
    in
    {
      packages.${system} = {
        default = self.packages.${system}.cli;
        cli = pkgs.callPackage ./nix/cli.nix (mkPackages pkgs);
      };
      # The endpoint is deployment configuration, not a credential. Keep it out
      # of this generic source, just as in the existing release/PKGBUILD flow.
      lib.mkClient =
        { pkgs, apiUrl }:
        pkgs.callPackage ./nix/client.nix (
          (mkPackages pkgs)
          // {
            inherit apiUrl;
            frontendSrc = sources.frontend;
          }
        );
      checks.${system} = {
        gui = self.lib.mkClient {
          inherit pkgs;
          apiUrl = "https://build.invalid/api";
        };
        service = import ./nix/test.nix { inherit pkgs self; };
      };
      nixosModules.default = import ./nix/module.nix { inherit self; };
      formatter.${system} = pkgs.nixfmt-tree;
    };
}
