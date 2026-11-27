{
  description = "Pods AirPods control and microphone capture for Linux";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      forSystems = nixpkgs.lib.genAttrs [ "x86_64-linux" "aarch64-linux" ];
    in {
      packages = forSystems (system:
        let pods = nixpkgs.legacyPackages.${system}.callPackage ./packaging/nix/package.nix { };
        in { inherit pods; default = pods; });
      overlays.default = final: _: {
        pods = final.callPackage ./packaging/nix/package.nix { };
      };
      homeManagerModules.default = import ./packaging/nix/home-manager.nix;
      checks = forSystems (system: { package = self.packages.${system}.pods; });
    };
}
