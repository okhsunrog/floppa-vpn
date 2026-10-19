{
  description = "Floppa VPN Linux packages and NixOS integration";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/fe13242281a67641c0102cdb1087f0ebe586c7df";
  outputs =
    { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs { inherit system; };
      # Recipe/documentation edits should not rebuild every Rust dependency.
      # self is a Git input: ignored credentials/build output never enter src.
      src = nixpkgs.lib.cleanSourceWith {
        src = self;
        name = "source";
        filter =
          path: _:
          let
            top = builtins.head (nixpkgs.lib.splitString "/" (nixpkgs.lib.removePrefix "${self}/" path));
          in
          !(builtins.elem top [
            "nix"
            "flake.nix"
            "flake.lock"
            "docs"
            ".github"
            "README.md"
          ]);
      };
    in
    {
      packages.${system} = {
        default = self.packages.${system}.cli;
        cli = pkgs.callPackage ./nix/cli.nix { inherit src; };
      };
      # The endpoint is deployment configuration, not a credential. Keep it out
      # of this generic source, just as in the existing release/PKGBUILD flow.
      lib.mkClient = { pkgs, apiUrl }: pkgs.callPackage ./nix/client.nix { inherit src apiUrl; };
      checks.${system}.service = import ./nix/test.nix { inherit pkgs self; };
      nixosModules.default = import ./nix/module.nix { inherit self; };
      formatter.${system} = pkgs.nixfmt-tree;
    };
}
