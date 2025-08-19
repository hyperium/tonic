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
      inputs = {
        nixpkgs.follows = "nixpkgs";
      };
    };
  };

  outputs =
    inputs@{ self, flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      imports = [
        inputs.git-hooks.flakeModule
      ];
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
        "x86_64-darwin"
      ];
      perSystem =
        {
          config,
          pkgs,
          system,
          ...
        }:
        let
          rustToolchain = pkgs.fenix.stable;
        in
        {
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
              clippy = {
                enable = true;
                packageOverrides = {
                  cargo = rustToolchain.cargo;
                  clippy = rustToolchain.clippy;
                };
              };
              cargo-check = {
                enable = true;
                package = rustToolchain.cargo;
                entry = "${rustToolchain.cargo}/bin/cargo check --workspace --all-features";
                files = "\\.rs$";
                pass_filenames = false;
              };
              rustfmt = {
                enable = true;
                packageOverrides = {
                  rustfmt = rustToolchain.rustfmt;
                  cargo = rustToolchain.cargo;
                };
              };
            };
          };

          devShells.default = pkgs.mkShell {
            packages = with pkgs; [
              cargo-nextest
              bazel_7
              pre-commit

              (rustToolchain.withComponents [
                "cargo"
                "clippy"
                "rust-src"
                "rustc"
                "rustfmt"
                "rust-analyzer"
              ])
              # protobuf
            ];

            hardeningDisable = [ "fortify" ];

            shellHook = ''
              export PATH="$PWD/protoc-gen-rust-grpc/bazel-bin/src:$HOME/code/install/bin:$PATH"

              ${config.pre-commit.installationScript}
            '';
          };
        };
    };
}
