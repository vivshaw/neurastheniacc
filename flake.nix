{
  description = "the groundbreaking new LLM-based C->WASM compiler you didn't need";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      perSystem = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          nativeInputs = with pkgs; [
            cmake
            rustPlatform.bindgenHook
          ];
          package = pkgs.rustPlatform.buildRustPackage {
            pname = "neurastheniacc";
            version = "0.1.0";
            src = pkgs.lib.fileset.toSource {
              root = ./.;
              fileset = pkgs.lib.fileset.unions [
                ./Cargo.toml
                ./Cargo.lock
                ./crates
                ./prompts
              ];
            };
            cargoLock.lockFile = ./Cargo.lock;
            nativeBuildInputs = nativeInputs;
            nativeCheckInputs = [ pkgs.clang ];
            # CMake is invoked by llama.cpp's Cargo build script, not as a top-level phase.
            dontUseCmakeConfigure = true;
            meta = {
              description = "the groundbreaking new LLM-based C->WASM compiler you didn't need";
              mainProgram = "neurastheniacc";
              license = pkgs.lib.licenses.mit;
              platforms = systems;
            };
          };
        in
        {
          inherit package;
          shell = pkgs.mkShell {
            packages = with pkgs; [
              git
              cargo
              rustc
              rustfmt
              clippy
              rust-analyzer
              clang
              cmake
              rustPlatform.bindgenHook
              wasmtime
              nixfmt
            ];
            RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
          };
          formatter = pkgs.nixfmt;
        }
      );
    in
    {
      packages = forAllSystems (system: {
        default = perSystem.${system}.package;
        neurastheniacc = perSystem.${system}.package;
      });
      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/neurastheniacc";
        };
        bench = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/neurastheniacc-bench";
        };
      });
      devShells = forAllSystems (system: {
        default = perSystem.${system}.shell;
      });
      checks = forAllSystems (system: {
        package = perSystem.${system}.package;
      });
      formatter = forAllSystems (system: perSystem.${system}.formatter);
    };
}
