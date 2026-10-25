{
  description = "amoeboid: NixOS microVMs for running agents against host dirs";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    microvm.url = "github:microvm-nix/microvm.nix";
    microvm.inputs.nixpkgs.follows = "nixpkgs";
    # Overridden by the launcher (--override-input) with ~/.config/amoeboid/host_<user>.nix.
    host = {
      url = "path:./host-default.nix";
      flake = false;
    };
  };

  # amoeboid copies this directory as a preset (~/.config/amoeboid/nix_presets/<name>),
  # then per VM. Any directory works as a preset if its flake takes a `host` input and
  # provides packages.x86_64-linux.default with `runner` and `virtiofsd`, and its NixOS
  # configuration keeps base.nix's contract (shares, extra-args file, control socket).
  outputs = { self, nixpkgs, microvm, host }:
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
          ./recipe.nix
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
