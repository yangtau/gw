{
  description = "tmux-native status panel for coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
  inputs.nix-prebuilt.url = "github:yangtau/nix-prebuilt";

  outputs =
    { self, nixpkgs, nix-prebuilt }:
    let
      inherit (nixpkgs) lib;
      systems = [ "aarch64-darwin" "aarch64-linux" "x86_64-linux" ];
      bins = [
        "gw"
        "gw-provider-claude"
        "gw-provider-codex"
        "gw-provider-amp"
        "gw-provider-opencode"
        "gw-provider-pi"
        "gw-provider-grok"
        "gw-provider-cursor"
      ];
      meta = {
        description = "tmux-native status panel for coding agents";
        homepage = "https://github.com/yangtau/gw";
        license = lib.licenses.mit;
      };
    in
    {
      packages = nix-prebuilt.lib.mkPackages {
        inherit self nixpkgs meta systems bins;
        pname = "gw";
        owner = "yangtau";
        repo = "gw";
        hashes = ./nix/prebuilt-hashes.json;
        fromSource =
          pkgs:
          pkgs.rustPlatform.buildRustPackage {
            pname = "gw";
            version = "0.1.0";
            src = self;
            cargoLock.lockFile = ./Cargo.lock;
            meta = meta // {
              platforms = systems;
              mainProgram = "gw";
            };
          };
      };

      devShells = lib.genAttrs systems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              rustc
              clippy
              rustfmt
              rust-analyzer
            ];
          };
        }
      );
    };
}
