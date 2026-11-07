{
  description = "j3code: native desktop + server for the omp coding agent";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-darwin"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});

      # GPUI on Linux: X11/Wayland windowing, xkbcommon keymaps, fontconfig/freetype
      # text, Vulkan rendering (loaded at runtime).
      linuxGuiLibs =
        pkgs: with pkgs; [
          fontconfig
          freetype
          libxkbcommon
          vulkan-loader
          wayland
          xorg.libX11
          xorg.libxcb
        ];

      # Dictation (j3-desktop): cpal records through ALSA (PipeWire's ALSA plugin on
      # NixOS); whisper-rs builds whisper.cpp with cmake, its bindings with bindgen,
      # and its Vulkan backend's shaders with glslc.
      speechNativeInputs =
        pkgs: with pkgs; [
          cmake
          shaderc
          rustPlatform.bindgenHook
        ];
      speechLibs =
        pkgs: with pkgs; [
          alsa-lib
          vulkan-headers
          vulkan-loader
        ];

      j3Package =
        pkgs:
        {
          pname,
          extra ? { },
        }:
        pkgs.rustPlatform.buildRustPackage (
          {
            inherit pname;
            version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
            src = pkgs.lib.fileset.toSource {
              root = ./.;
              fileset = pkgs.lib.fileset.unions [
                ./Cargo.toml
                ./Cargo.lock
                ./crates
              ];
            };
            cargoLock = {
              lockFile = ./Cargo.lock;
              # omp-rpc is a git dependency pinned by rev in Cargo.toml.
              allowBuiltinFetchGit = true;
            };
            cargoBuildFlags = [
              "-p"
              pname
            ];
            # Tests run in the dev shell; the live omp test needs credentials anyway.
            doCheck = false;
          }
          // extra
        );
    in
    {
      packages = forAllSystems (
        pkgs:
        {
          j3-server = j3Package pkgs {
            pname = "j3-server";
            extra.meta.mainProgram = "j3-server";
          };
          default = self.packages.${pkgs.stdenv.hostPlatform.system}.j3-server;
        }
        # The Mac builds the desktop with cargo (Metal, Apple SDK); Nix packages it for Linux.
        // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
          j3-desktop = j3Package pkgs {
            pname = "j3-desktop";
            extra = {
              nativeBuildInputs = [
                pkgs.pkg-config
                pkgs.makeWrapper
                pkgs.copyDesktopItems
                pkgs.imagemagick
              ]
              ++ speechNativeInputs pkgs;
              buildInputs = linuxGuiLibs pkgs ++ speechLibs pkgs;
              # A portable x86-64-v3 whisper.cpp rather than one tuned to the build machine.
              env = {
                GGML_NATIVE = "OFF";
                GGML_AVX = "ON";
                GGML_AVX2 = "ON";
                GGML_FMA = "ON";
                GGML_F16C = "ON";
                # The source set has no .git; build.rs reads the commit from here.
                J3_GIT_COMMIT = self.shortRev or self.dirtyShortRev or "unknown";
              };
              # Named after the window's app id (APP_ID in crates/j3-desktop/src/main.rs),
              # so the compositor ties the window to this entry and its icon.
              desktopItems = [
                (pkgs.makeDesktopItem {
                  name = "dev.jacobyuill.j3code";
                  desktopName = "j3";
                  comment = "Desktop client for the omp coding agent";
                  exec = "j3";
                  icon = "dev.jacobyuill.j3code";
                  categories = [ "Development" ];
                  startupWMClass = "dev.jacobyuill.j3code";
                })
              ];
              # Icon: the iOS app icon, as in contrib/macos/bundle.sh.
              postInstall = ''
                for size in 128 256 512; do
                  dir=$out/share/icons/hicolor/''${size}x''${size}/apps
                  mkdir -p $dir
                  magick ${./apps/mobile/assets/brand/ios-icon-1024.png} -resize ''${size}x''${size} \
                    $dir/dev.jacobyuill.j3code.png
                done
              '';
              postFixup = ''
                wrapProgram $out/bin/j3 \
                  --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath (linuxGuiLibs pkgs)}
              '';
              meta.mainProgram = "j3";
            };
          };
        }
      );

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell (
          {
            packages = with pkgs; [
              cargo
              rustc
              clippy
              rustfmt
              rust-analyzer
              cmake
            ];
            RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
          }
          // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
            nativeBuildInputs = [ pkgs.pkg-config ] ++ speechNativeInputs pkgs;
            buildInputs = linuxGuiLibs pkgs ++ speechLibs pkgs;
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (linuxGuiLibs pkgs);
          }
        );
      });

      formatter = forAllSystems (pkgs: pkgs.nixfmt-rfc-style);
    };
}
