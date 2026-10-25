{
  description = "amoeboid: NixOS microVMs for running agents against host dirs";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    microvm.url = "github:microvm-nix/microvm.nix";
    microvm.inputs.nixpkgs.follows = "nixpkgs";
    # Both are overridden by the launcher (--override-input): `recipe` with the recipe
    # it was given, `host` with ~/.config/amoeboid/host_<user>.nix. A file is imported
    # as-is, a directory via its default.nix.
    recipe = {
      url = "path:./recipe.nix";
      flake = false;
    };
    host = {
      url = "path:./host-default.nix";
      flake = false;
    };
  };

  outputs = { self, nixpkgs, microvm, recipe, host }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
    in
    {
      nixosConfigurations.vm = nixpkgs.lib.nixosSystem {
        modules = [
          { nixpkgs.hostPlatform = system; }
          microvm.nixosModules.microvm
          ./base.nix
          "${recipe}"
          "${host}"
        ];
      };

      # Everything the host side needs: the VM runner plus virtiofsd for the shares.
      packages.${system}.default = pkgs.linkFarm "amoeboid-vm" {
        runner = self.nixosConfigurations.vm.config.microvm.declaredRunner;
        virtiofsd = pkgs.virtiofsd;
      };
    };
}
