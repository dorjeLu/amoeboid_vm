# The recipe: a NixOS module describing what's in the VM (packages, services, tweaks).
# Edit it in a preset (~/.config/amoeboid/nix_presets/<name>/) for new VMs, or in a VM's
# own copy (<data dir>/amoeboid/<vm>/nix/) for that VM.
{ config, pkgs, ... }:

{
  # Size (base.nix defaults: 8 vCPUs, 8192 MiB; avoid exactly 2048 MiB, qemu hangs)
  # microvm.vcpu = 8;
  # microvm.mem = 8192;

  environment.systemPackages = with pkgs; [
    # Agents
    opencode

    # Toolchains
    mise
    rustup # toolchains land in ~/.rustup and follow rust-toolchain.toml
    gcc # linker for rustc
    gnumake
    pkg-config
    openssl
    python3
    nodejs # npx, and the npm-distributed agent CLIs

    # Search and text; agents should reach for rg/fd first, grep/find remain for scripts
    ripgrep
    fd
    jq
    yq-go
    gnugrep
    gnused
    gawk
    findutils
    diffutils
    patch
    tree
    file
    less
    which
    bc

    # Version control and network
    git
    gh
    curl
    wget
    openssh
    rsync
    iproute2
    iputils
    dnsutils
    netcat-openbsd

    # Archives
    gnutar
    gzip
    bzip2
    xz
    zstd
    zip
    unzip

    # Processes and debugging
    procps
    psmisc
    htop
    lsof
    strace

    # Editors and sessions
    vim
    nano
    tmux
  ];

  # Lets prebuilt binaries (mise-installed tools, npm/pip wheels) run on NixOS.
  programs.nix-ld.enable = true;

  # Have pkg-config find openssl and friends for -sys crates.
  environment.variables.PKG_CONFIG_PATH = "/run/current-system/sw/lib/pkgconfig";
  environment.extraOutputsToInstall = [ "dev" ];

  # nixpkgs' mise compiles node and python from source by default (minutes, and a
  # silent-looking hang for agents); with nix-ld the official prebuilt ones just work.
  environment.variables = {
    MISE_NODE_COMPILE = "0";
    MISE_PYTHON_COMPILE = "0";
  };

  programs.fish.enable = true;
  programs.fish.interactiveShellInit = "mise activate fish | source";
  users.users.${config.amoeboid.user}.shell = pkgs.fish;
}
