{
  description = "Cross-platform Rust GPU warping module";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay.url = "github:oxalica/rust-overlay";
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];

        pkgs = import nixpkgs {
          inherit system overlays;
        };

        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [
            "rust-src"
            "rustfmt"
            "clippy"
          ];
          targets = [
            "x86_64-unknown-linux-gnu"
            "aarch64-unknown-linux-gnu"
            "x86_64-pc-windows-gnu"
            "aarch64-apple-darwin"
            "x86_64-apple-darwin"
          ];
        };

        commonDeps = with pkgs; [
          rustToolchain
          pkg-config
          clang
          llvmPackages.bintools
          sdl2

        ];

        linuxDeps = with pkgs; [
          vulkan-loader
          vulkan-headers
          wayland
          libxkbcommon
        ];

        darwinDeps = with pkgs; [
          darwin.apple_sdk.frameworks.Metal
          darwin.apple_sdk.frameworks.Foundation
        ];

        windowsDeps = with pkgs; [
          mingw_w64
        ];

      in {
        devShells.default = pkgs.mkShell {
          buildInputs =
            commonDeps
            ++ pkgs.lib.optionals pkgs.stdenv.isLinux linuxDeps
            ++ pkgs.lib.optionals pkgs.stdenv.isDarwin darwinDeps
            ++ pkgs.lib.optionals pkgs.stdenv.isWindows windowsDeps;

          RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";

          shellHook = ''
            echo "Rust GPU dev shell (${system})"
          '';
        };

        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "gpu-remap";
          version = "0.1.0";

          src = ./.;

          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = commonDeps;

          buildInputs =
            pkgs.lib.optionals pkgs.stdenv.isLinux linuxDeps
            ++ pkgs.lib.optionals pkgs.stdenv.isDarwin darwinDeps
            ++ pkgs.lib.optionals pkgs.stdenv.isWindows windowsDeps;

          doCheck = false;
        };
      }
    );
}

