# Plumbing shared by every recipe: user, shares, networking, ssh, console.
# Recipes shouldn't need to touch this; put packages and tweaks in the recipe.
{ config, lib, pkgs, ... }:

let
  cfg = config.amoeboid;
  # Per-VM state dir from the host (<data dir>/amoeboid/<name>/guest). The launcher
  # writes the files read below into it before every boot.
  state = "/mnt/amoeboid";
  home = "/home/${cfg.user}";
in
{
  options.amoeboid = {
    user = lib.mkOption {
      type = lib.types.str;
      description = "Guest user; matches the host user owning the shared dirs.";
    };
    uid = lib.mkOption { type = lib.types.int; };
    gid = lib.mkOption { type = lib.types.int; };
  };

  config = {
    system.stateVersion = "26.05";
    networking.hostName = lib.mkDefault "amoeboid";

    microvm = {
      hypervisor = "qemu";
      vcpu = lib.mkDefault 8;
      mem = lib.mkDefault 8192;
      # QMP, used by `amoeboid stop`; the name must match VmPaths.control_socket.
      socket = "control.sock";

      shares = [
        {
          # Host store, read-only: packages are shared, not copied.
          proto = "virtiofs";
          tag = "ro-store";
          source = "/nix/store";
          mountPoint = "/nix/.ro-store";
          socket = "ro-store.sock";
        }
        {
          proto = "virtiofs";
          tag = "amoeboid";
          source = "amoeboid-state"; # chosen at runtime by the launcher
          mountPoint = state;
          socket = "state.sock";
        }
      ];

      # Nothing that varies per start is part of the build: the launcher writes the
      # network device (user-mode, with the ssh port forward) and one vhost-user-fs
      # device per shared dir to this file in the runtime dir (the runner's cwd).
      interfaces = [ ];
      extraArgsScript = "${pkgs.writeShellScript "amoeboid-extra-args" ''
        cat amoeboid-qemu-args 2>/dev/null || true
      ''}";
    };

    users.groups.${cfg.user}.gid = cfg.gid;
    users.users.${cfg.user} = {
      isNormalUser = true;
      inherit (cfg) uid;
      group = cfg.user;
      extraGroups = [ "wheel" ];
      inherit home;
      createHome = false; # lives in the state share, created by the host
      # Empty password: the console logs in automatically and ssh needs no credentials.
      # The ssh port is only reachable from the host's loopback.
      hashedPassword = "";
    };
    security.sudo.wheelNeedsPassword = false;

    fileSystems.${home} = {
      device = "${state}/home";
      fsType = "none";
      options = [ "bind" "x-systemd.requires-mounts-for=${state}" ];
    };

    # Before anything logs in: set the hostname, install the ssh host key, and mount
    # the dirs listed in the state share. Each line of `mounts` is
    # <tag>\t<rw|ro>\t<path>, parents before children.
    systemd.services.amoeboid-mounts = {
      wantedBy = [ "multi-user.target" ];
      before = [ "serial-getty@ttyS0.service" "sshd.service" "sshd-keygen.service" ];
      unitConfig.RequiresMountsFor = [ state home ];
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
      };
      path = [ pkgs.util-linux pkgs.coreutils pkgs.nettools ];
      script = ''
        [ -r ${state}/hostname ] && hostname "$(cat ${state}/hostname)"
        if [ -r ${state}/ssh_host_ed25519_key ]; then
          install -D -m 600 -o root -g root ${state}/ssh_host_ed25519_key /etc/ssh/ssh_host_ed25519_key
        fi
        [ -r ${state}/mounts ] || exit 0
        while IFS=$'\t' read -r tag mode path; do
          mkdir -p "$path"
          mount -t virtiofs -o "$mode" "$tag" "$path"
        done < ${state}/mounts
      '';
    };

    # ssh without credentials for now: the client's "none" auth succeeds because the
    # user's password is empty. Keys in ~/.ssh/authorized_keys work too, which is the
    # way to lock this down later (set PermitEmptyPasswords = false).
    services.openssh = {
      enable = true;
      # The launcher-provided key (installed above), so the host can pin it.
      hostKeys = [ { type = "ed25519"; path = "/etc/ssh/ssh_host_ed25519_key"; } ];
      settings = {
        PermitRootLogin = "no";
        AllowUsers = [ cfg.user ];
        PasswordAuthentication = true;
        PermitEmptyPasswords = true;
        KbdInteractiveAuthentication = false;
      };
    };
    security.pam.services.sshd.allowNullPassword = true;

    # Serial console: log straight in, with a sane TERM and the host terminal's size.
    services.getty.autologinUser = cfg.user;
    systemd.services."serial-getty@ttyS0".environment.TERM = "xterm-256color";
    environment.loginShellInit = ''
      if [ "$(tty)" = /dev/ttyS0 ] && [ -r ${state}/term ]; then
        stty $(cat ${state}/term)
      fi
    '';
  };
}
