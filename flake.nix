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

        # Mesa GL/Vulkan drivers for running the egui/wgpu GUI from the dev
        # shell on a non-NixOS host. The shell's LD_LIBRARY_PATH shadows the
        # host's libGL/libvulkan with nix's, and a nix-glibc process can't load
        # the host's driver .so's (two glibcs in one process crash). So we point
        # the loaders at nix-built Mesa instead, which talks to /dev/dri via the
        # kernel's stable ABI. Linux-only; empty elsewhere.
        graphicsLibs = pkgs.lib.optionals pkgs.stdenv.isLinux [
          pkgs.mesa           # DRI drivers (hardware iris/crocus + software swrast/llvmpipe)
          pkgs.vulkan-loader  # libvulkan.so.1 that wgpu dlopens
        ];

        # All x86_64 Vulkan ICDs nix Mesa ships (hardware for whatever GPU is
        # present, plus lavapipe as a software fallback). Discovered rather than
        # hardcoded so this isn't tied to one GPU vendor.
        mesaVulkanICDs =
          let
            dir = "${pkgs.mesa}/share/vulkan/icd.d";
            names = builtins.attrNames (builtins.readDir dir);
            x64 = builtins.filter (n: pkgs.lib.hasSuffix "x86_64.json" n) names;
          in
          pkgs.lib.concatMapStringsSep ":" (n: "${dir}/${n}") x64;

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

        devShells.default = pkgs.mkShell ({
          buildInputs = [ rust ] ++ libs ++ graphicsLibs;
          nativeBuildInputs = [ pkgs.pkg-config ];
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (libs ++ graphicsLibs);
          shellHook = ''
            echo "clipboard-tool dev shell — $(rustc --version)"
          '';
        } // pkgs.lib.optionalAttrs pkgs.stdenv.isLinux {
          # Point the GL and Vulkan loaders at nix Mesa so the wgpu-backed egui
          # window can create a surface (see graphicsLibs above).
          LIBGL_DRIVERS_PATH = "${pkgs.mesa}/lib/dri";
          __EGL_VENDOR_LIBRARY_DIRS = "${pkgs.mesa}/share/glvnd/egl_vendor.d";
          VK_ICD_FILENAMES = mesaVulkanICDs;
        });
      });
}
