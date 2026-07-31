{
  description = "tmux-native status panel for coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "aarch64-darwin" "x86_64-darwin" "aarch64-linux" "x86_64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (pkgs: rec {
        gw = pkgs.rustPlatform.buildRustPackage {
          pname = "gw";
          version = "0.1.0";
          src = self;
          cargoLock.lockFile = ./Cargo.lock;
          meta = {
            description = "tmux-native status panel for coding agents";
            homepage = "https://github.com/yangtau/gw";
            license = pkgs.lib.licenses.mit;
            platforms = systems;
            mainProgram = "gw";
          };
        };
        default = gw;
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [ cargo rustc clippy rustfmt rust-analyzer ];
        };
      });
    };
}
