{
  description = "Konnect — named local routes for Kubernetes port forwards";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.05";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
      forAllSystems = f:
        nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (pkgs: rec {
        konnect = pkgs.rustPlatform.buildRustPackage {
          pname = "konnect";
          version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;
          src = self;
          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = [ pkgs.makeWrapper ];

          # konnect shells out to kubectl, so pin one rather than relying on the
          # host having a compatible version installed.
          postInstall = ''
            wrapProgram $out/bin/konnect \
              --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.kubectl ]}
          '';

          meta = {
            description = "Named local routes for Kubernetes port forwards";
            homepage = "https://github.com/alexdaima/konnect";
            license = pkgs.lib.licenses.mit;
            mainProgram = "konnect";
            # The toolbar is macOS only; other platforms get `konnect init` and
            # `konnect list`, and a clear error from `konnect start`.
            platforms = pkgs.lib.platforms.unix;
          };
        };

        default = konnect;
      });
    };
}
