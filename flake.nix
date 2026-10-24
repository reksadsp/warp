{
  description = "Warp Rust development environments";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        rust = with pkgs; [ cargo rustc clippy rustfmt ];
        unixWarpNative = with pkgs; [ clang pkg-config ];
        unixWarpLibraries = with pkgs;
          lib.optionals stdenv.isLinux [
            dbus
            ffmpeg-full
            glib
            intel-media-driver
            libxcb
            libgbm
            libglvnd
            libxkbcommon
            libxrandr
            mesa
            pipewire
            wayland
            libx11
            libxi
            libxtst
            xdg-desktop-portal
          ];
      in {
        devShells.default = pkgs.mkShell {
          packages = rust;
          RUST_BACKTRACE = "1";
        };

        # Linux capture uses PipeWire/the desktop portal. macOS uses the
        # system ScreenCaptureKit framework, so no Linux dependencies leak
        # into Darwin shells.
        devShells.unix-warp = pkgs.mkShell {
          packages = rust ++ unixWarpNative ++ unixWarpLibraries;
          RUST_BACKTRACE = "1";
          LIBCLANG_PATH = pkgs.lib.makeLibraryPath [ pkgs.llvmPackages.libclang.lib ];
          LD_LIBRARY_PATH = pkgs.lib.optionalString pkgs.stdenv.isLinux
            (pkgs.lib.makeLibraryPath unixWarpLibraries);
          LIBVA_DRIVERS_PATH = pkgs.lib.optionalString pkgs.stdenv.isLinux
            "${pkgs.intel-media-driver}/lib/dri";
        };

      });
}
