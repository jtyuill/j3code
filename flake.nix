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
    in
    {
      packages = forAllSystems (pkgs: {
        j3-server = pkgs.rustPlatform.buildRustPackage {
          pname = "j3-server";
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
          cargoBuildFlags = [ "-p" "j3-server" ];
          # Tests run in the dev shell; the live omp test needs credentials anyway.
          doCheck = false;
          meta.mainProgram = "j3-server";
        };
        default = self.packages.${pkgs.stdenv.hostPlatform.system}.j3-server;
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            rustc
            clippy
            rustfmt
            rust-analyzer
          ];
          RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
        };
      });

      formatter = forAllSystems (pkgs: pkgs.nixfmt-rfc-style);
    };
}
