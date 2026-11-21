{
  description = "disk-space — cross-platform Native Shell (nsh) storage explorer and secure erasure tool";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
      flake-utils,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };
        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        rustPlatform = pkgs.makeRustPlatform {
          cargo = rustToolchain;
          rustc = rustToolchain;
        };

        nativeBuildInputs = with pkgs; [
          pkg-config
        ];

        buildInputs =
          with pkgs;
          lib.optionals stdenv.hostPlatform.isLinux [
            gtk4
            pango
            glib
            cairo
            gdk-pixbuf
            graphene
          ]
          ++ lib.optionals stdenv.hostPlatform.isDarwin [
            libiconv
          ];

        disk-space = rustPlatform.buildRustPackage {
          pname = "disk-space";
          version = "0.1.0";
          src = self;

          cargoLock = {
            lockFile = ./Cargo.lock;
          };

          inherit nativeBuildInputs buildInputs;

          cargoBuildFlags = [ "--workspace" ];
          cargoTestFlags = [ "--workspace" ];

          meta = with pkgs.lib; {
            description = "Cross-platform storage explorer and secure erasure tool built with Native Shell";
            mainProgram = "disk-space-app";
            platforms = platforms.unix ++ platforms.windows;
          };
        };
      in
      {
        packages = {
          default = disk-space;
          inherit disk-space;
        };

        apps.default = flake-utils.lib.mkApp {
          drv = disk-space;
          name = "disk-space-app";
        };

        devShells.default = pkgs.mkShell {
          inherit nativeBuildInputs buildInputs;
          packages = with pkgs; [
            rustToolchain
            git
          ];

          env = {
            RUST_BACKTRACE = "1";
          }
          // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath buildInputs;
          };

          shellHook = ''
            echo "disk-space · $(rustc --version)"
          '';
        };

        checks = {
          package = disk-space;

          fmt = pkgs.runCommand "disk-space-fmt" { nativeBuildInputs = [ rustToolchain ]; } ''
            cd ${self}
            cargo fmt --all -- --check
            touch $out
          '';

          clippy = disk-space.overrideAttrs (old: {
            pname = "disk-space-clippy";
            doCheck = false;
            buildPhase = ''
              runHook preBuild
              cargo clippy --workspace --all-targets --offline -- -D warnings
              runHook postBuild
            '';
            installPhase = ''
              touch $out
            '';
          });
        };
      }
    );
}
