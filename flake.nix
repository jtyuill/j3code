{
  description = "j3code: native desktop + server for the omp coding agent";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    # Crane builds the dependencies as their own derivation, so a code change
    # recompiles only the workspace crates.
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    {
      self,
      nixpkgs,
      crane,
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-darwin"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});

      # Rune on Linux: winit's Wayland/X11 windowing (xkbcommon keymaps; the X11 libraries it
      # opens at runtime) and wgpu's Vulkan rendering (loaded at runtime). Text needs no system
      # library; Rune asks `fc-match` for the monospace family.
      linuxGuiLibs =
        pkgs: with pkgs; [
          libxkbcommon
          vulkan-loader
          wayland
          xorg.libX11
          xorg.libXcursor
          xorg.libXi
          xorg.libXrandr
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

      # `build` is the environment both derivations compile in: it must match
      # exactly, or cargo rebuilds the dependencies whose build scripts watch it.
      # `install` applies to the final package only.
      j3Package =
        pkgs:
        {
          pname,
          build ? { },
          install ? { },
        }:
        let
          craneLib = crane.mkLib pkgs;
          common = {
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
            cargoExtraArgs = "--locked -p ${pname}";
            # Tests run in the dev shell; the live omp test needs credentials anyway.
            doCheck = false;
          }
          // build;
        in
        craneLib.buildPackage (
          common
          // {
            cargoArtifacts = craneLib.buildDepsOnly (
              common
              // {
                # Only the release build is reused; skip crane's extra `cargo check`.
                cargoCheckCommand = "true";
              }
            );
          }
          // install
        );

      desktopBuild = pkgs: {
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
        };
        # bindgenHook copies NIX_CFLAGS_COMPILE into BINDGEN_EXTRA_CLANG_ARGS, including
        # the per-derivation -frandom-seed; whisper-rs-sys watches that variable, so the
        # seed would rebuild whisper.cpp in the final package. It doesn't affect bindings.
        preBuild = ''
          export BINDGEN_EXTRA_CLANG_ARGS=$(sed 's/-frandom-seed=[^ ]*//g' <<< "$BINDGEN_EXTRA_CLANG_ARGS")
        '';
      };
    in
    {
      packages = forAllSystems (
        pkgs:
        {
          j3-server = j3Package pkgs {
            pname = "j3-server";
            install.meta.mainProgram = "j3-server";
          };
          default = self.packages.${pkgs.stdenv.hostPlatform.system}.j3-server;
        }
        # The Mac builds the desktop with cargo (Metal, Apple SDK); Nix packages it for Linux.
        // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
          j3-desktop = j3Package pkgs {
            pname = "j3-desktop";
            build = desktopBuild pkgs;
            install = {
              # The source set has no .git; build.rs reads the commit from here. Only
              # the final package sees it, so a new commit leaves the dependencies cached.
              env = (desktopBuild pkgs).env // {
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
                  magick ${./assets/icon-1024.png} -resize ''${size}x''${size} \
                    $dir/dev.jacobyuill.j3code.png
                done
              '';
              postFixup = ''
                wrapProgram $out/bin/j3 \
                  --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath (linuxGuiLibs pkgs)} \
                  --suffix PATH : ${pkgs.lib.makeBinPath [ pkgs.fontconfig ]}
              '';
              meta.mainProgram = "j3";
            };
          };
        }
      );

      devShells = forAllSystems (pkgs: {
        # mold links the desktop binary in a second or two; GNU ld takes ~20.
        default =
          (
            if pkgs.stdenv.hostPlatform.isLinux then
              pkgs.mkShell.override { stdenv = pkgs.stdenvAdapters.useMoldLinker pkgs.stdenv; }
            else
              pkgs.mkShell
          )
            (
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
