{
  description = "clipboard-tool — lightweight cross-platform clipboard history manager";

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

        # Native libraries the Linux backends link against and dlopen at runtime.
        # Empty on non-Linux so macOS/other systems still evaluate.
        libs = pkgs.lib.optionals pkgs.stdenv.isLinux (with pkgs; [
          libGL
          libxkbcommon
          wayland
          libx11
          libxcursor
          libxrandr
          libxi
          libxtst
          libxcb
          xdotool                      # provides libxdo (enigo X11)
          libei                        # enigo Wayland/libei backend
          gtk3                         # tray-icon on Linux
          libayatana-appindicator      # tray-icon on Linux
        ]);

        rustPlatform = pkgs.makeRustPlatform {
          cargo = rust;
          rustc = rust;
        };

        clipboard-tool = rustPlatform.buildRustPackage {
          pname = "clipboard-tool";
          version = "0.1.0";
          src = pkgs.lib.cleanSourceWith {
            src = ./.;
            # Keep target/ and result/ out of the store-copied source.
            filter = path: type:
              let base = baseNameOf path;
              in base != "target" && base != "result"
                 && pkgs.lib.cleanSourceFilter path type;
          };
          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = [ pkgs.pkg-config ]
            ++ pkgs.lib.optional pkgs.stdenv.isLinux pkgs.makeWrapper;
          buildInputs = libs;

          # GUI/Wayland libs are dlopen'd at runtime, so put them on the
          # wrapped binary's library path.
          postInstall = pkgs.lib.optionalString pkgs.stdenv.isLinux ''
            wrapProgram $out/bin/clipboard-tool \
              --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath libs}
          '';

          meta = {
            description = "Lightweight cross-platform clipboard history manager";
            mainProgram = "clipboard-tool";
          };
        };
      in {
        packages.default = clipboard-tool;

        devShells.default = pkgs.mkShell {
          buildInputs = [ rust ] ++ libs;
          nativeBuildInputs = [ pkgs.pkg-config ];
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath libs;
          shellHook = ''
            echo "clipboard-tool dev shell — $(rustc --version)"
          '';
        };
      });
}
