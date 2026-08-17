{
  description = "tmux-native status panel for coding agents";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs = { self, nixpkgs }:
    let
      inherit (nixpkgs) lib;
      systems = [ "aarch64-darwin" "aarch64-linux" "x86_64-linux" ];
      forAllSystems = f: lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
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
      prebuilt = builtins.fromJSON (builtins.readFile ./nix/prebuilt-hashes.json);

      meta = pkgs: {
        description = "tmux-native status panel for coding agents";
        homepage = "https://github.com/yangtau/gw";
        license = pkgs.lib.licenses.mit;
        platforms = systems;
        mainProgram = "gw";
      };

      gwFromSource = pkgs: pkgs.rustPlatform.buildRustPackage {
        pname = "gw";
        version = "0.1.0";
        src = self;
        cargoLock.lockFile = ./Cargo.lock;
        meta = meta pkgs;
      };

      gwPrebuilt = pkgs: system: hash: pkgs.stdenv.mkDerivation {
        pname = "gw";
        version = builtins.substring 0 7 prebuilt.rev;
        src = pkgs.fetchurl {
          url = "https://github.com/yangtau/gw/releases/download/prebuilt/gw-${system}-${prebuilt.rev}.tar.gz";
          inherit hash;
        };
        nativeBuildInputs = lib.optionals pkgs.stdenv.isLinux [ pkgs.autoPatchelfHook ];
        buildInputs = lib.optionals pkgs.stdenv.isLinux [ pkgs.stdenv.cc.cc.lib ];
        dontUnpack = true;
        dontConfigure = true;
        dontBuild = true;
        dontStrip = true;
        installPhase = ''
          runHook preInstall
          mkdir -p $out/bin
          tar -xzf $src -C $out/bin
          for bin in ${lib.concatStringsSep " " bins}; do
            test -x "$out/bin/$bin"
          done
          runHook postInstall
        '';
        meta = meta pkgs;
      };
    in
    {
      packages = forAllSystems (pkgs:
        let
          system = pkgs.stdenv.hostPlatform.system;
          fromSource = gwFromSource pkgs;
          hash = prebuilt.hashes.${system} or null;
          # Clean trees download the last CI tarball. Dirty trees compile.
          usePrebuilt = hash != null && self ? rev;
          pkg = if usePrebuilt then gwPrebuilt pkgs system hash else fromSource;
        in
        {
          gw = pkg;
          gw-from-source = fromSource;
          default = pkg;
        });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [ cargo rustc clippy rustfmt rust-analyzer ];
        };
      });
    };
}
