{
  description = "Description for the project";

  inputs = {
    flake-parts.url = "github:hercules-ci/flake-parts";
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    git-hooks = {
      url = "github:cachix/git-hooks.nix";
      inputs = { nixpkgs.follows = "nixpkgs"; };
    };

    treefmt-nix = {
      url = "github:numtide/treefmt-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = inputs@{ self, flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      imports = [ inputs.git-hooks.flakeModule inputs.treefmt-nix.flakeModule ];
      systems =
        [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" "x86_64-darwin" ];
      perSystem = { config, pkgs, system, ... }:
        let rustToolchain = pkgs.fenix.stable;
        in {
          _module.args.pkgs = import inputs.nixpkgs {
            inherit system;
            overlays = [ inputs.fenix.overlays.default ];
            config = { };
          };

          formatter = config.treefmt.build.wrapper;
          checks.formatting = config.treefmt.build.check self;

          pre-commit = {
            check.enable = true;
            settings.hooks = {
              actionlint.enable = true;
              shellcheck.enable = true;
              treefmt.enable = true;
            };
          };

          treefmt = {
            settings = { rustfmt.enable = true; };
            projectRootFile = ".git/config";
            flakeCheck = false; # Covered by git-hooks check
          };

          devShells.default = pkgs.mkShell {
            packages = with pkgs; [
              nixd
              nixfmt

              (rustToolchain.withComponents [
                "cargo"
                "clippy"
                "rust-src"
                "rustc"
                "rustfmt"
                "rust-analyzer"
              ])
              protobuf
            ];
          };
        };
    };
}
