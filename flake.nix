{
  description = "clipboard-tool dev shell — lightweight cross-platform clipboard history manager";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };

        rust = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" "clippy" "rustfmt" ];
        };

        # Native libraries the Linux backends link against and load at runtime.
        # Only meaningful on Linux; empty elsewhere so macOS/other systems still eval.
        libs = pkgs.lib.optionals pkgs.stdenv.isLinux (with pkgs; [
          libGL
          libxkbcommon
          wayland
          xorg.libX11
          xorg.libXcursor
          xorg.libXrandr
          xorg.libXi
          xorg.libXtst
          xorg.libxcb
          libei                        # enigo Wayland/libei backend
          gtk3                         # tray-icon on Linux
          libayatana-appindicator      # tray-icon on Linux
        ]);
      in {
        devShells.default = pkgs.mkShell {
          buildInputs = [ rust ] ++ libs;
          nativeBuildInputs = with pkgs; [ pkg-config ]
            ++ pkgs.lib.optionals pkgs.stdenv.isLinux [ pkgs.xdotool ];

          # On non-NixOS the GUI + Wayland libs must be resolvable at runtime.
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath libs;

          shellHook = ''
            echo "clipboard-tool dev shell — $(rustc --version)"
          '';
        };
      });
}
