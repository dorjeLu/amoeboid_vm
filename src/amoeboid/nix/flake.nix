{
  description = "amoeboid: NixOS microVMs for running agents against host dirs";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    microvm.url = "github:microvm-nix/microvm.nix";
    microvm.inputs.nixpkgs.follows = "nixpkgs";
    # Overridden by the launcher (--override-input) with the guest account it renders
    # from run_config.kdl on every start (<data dir>/amoeboid/<vm>/host.nix).
    host = {
      url = "path:./host-default.nix";
      flake = false;
    };
  };

  # amoeboid copies this directory as a preset (~/.config/amoeboid/nix_presets/<name>),
  # then per VM. Any directory works as a preset if its flake takes a `host` input and
  # provides packages.<host system>.default with `runner` and `virtiofsd`, and its NixOS
  # configuration keeps base.nix's contract (shares, extra-args file, control socket).
  # The guest has the host's architecture: `nix build` picks the host system's package.
  outputs = { self, nixpkgs, microvm, host }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      vm = system: nixpkgs.lib.nixosSystem {
        modules = [
          { nixpkgs.hostPlatform = system; }
          microvm.nixosModules.microvm
          ./base.nix
          ./recipe.nix
          "${host}"
        ];
      };
    in
    {
      nixosConfigurations = nixpkgs.lib.genAttrs systems vm;

      # Everything the host side needs: the VM runner plus virtiofsd for the shares.
      packages = nixpkgs.lib.genAttrs systems (system:
        let pkgs = nixpkgs.legacyPackages.${system}; in {
          default = pkgs.linkFarm "amoeboid-vm" {
            runner = self.nixosConfigurations.${system}.config.microvm.declaredRunner;
            virtiofsd = pkgs.virtiofsd;
          };
        });
    };
}
