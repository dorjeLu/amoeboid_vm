# Placeholder for the `host` flake input so the flake evaluates on its own (nix flake
# check/lock). The launcher always replaces it with the guest account it renders from
# run_config.kdl.
{ ... }:

{
  amoeboid.user = "amoeboid";
  amoeboid.uid = 1000;
  amoeboid.gid = 1000;
  amoeboid.home = "/home/amoeboid";
}
