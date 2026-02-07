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
              pre-commit
              cmake

              (rustToolchain.withComponents [
                "cargo"
                "clippy"
                "rust-src"
                "rustc"
                "rustfmt"
                "rust-analyzer"
              ])
            ];

            hardeningDisable = [ "fortify" ];

            shellHook = ''
              ${config.pre-commit.installationScript}
            '';
          };

          # App for running the full CI udeps check
          # `nix run .#udeps-ci`
          apps.udeps-ci = {
            type = "app";
            program = "${pkgs.writeShellScript "udeps-ci" ''
              set -e
              export PATH="${pkgs.rustup}/bin:${pkgs.cargo-udeps}/bin:${pkgs.cargo-hack}/bin:$PATH"

              # Ensure nightly toolchain is installed
              if ! rustup toolchain list | grep -q "nightly-2025-03-27"; then
                echo "Installing nightly-2025-03-27 toolchain..."
                rustup toolchain install nightly-2025-03-27
              fi

              # Set the toolchain for this run
              export RUSTUP_TOOLCHAIN=nightly-2025-03-27

              echo "Running cargo hack udeps..."
              cargo hack udeps --workspace --exclude-features=_tls-any,tls,tls-aws-lc,tls-ring,tls-connect-info --each-feature

              echo "Running tonic TLS feature checks..."
              cargo udeps --package tonic --features tls-ring,transport
              cargo udeps --package tonic --features tls-ring,server
              cargo udeps --package tonic --features tls-ring,channel
              cargo udeps --package tonic --features tls-aws-lc,transport
              cargo udeps --package tonic --features tls-aws-lc,server
              cargo udeps --package tonic --features tls-aws-lc,channel
              cargo udeps --package tonic --features tls-connect-info

              echo "✓ All udeps checks passed!"
            ''}";
          };
        };
    };
}
