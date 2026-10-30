//! `RunConfig`: the run configuration as values. Every field is a refined type, so a `RunConfig`
//! is valid by construction; `run_config.kdl` <-> `RunConfig` is `decode`/`encode`.

use crate::domain::{
    AbsDir, BaseSpec, Gid, HomeDir, HomeRel, HomeStorage, LingerPolicy, MountRequest, PresetName,
    Seconds, SshPort, Uid, UserName,
};
use crate::refined;

/// Comment at the top of every written file (the only comment kept when amoeboid rewrites it).
pub const HEADER: &str = "\
// amoeboid run configuration (KDL v2). Read once per command, never hot-reloaded.
// amoeboid rewrites this file when it saves a setting itself, keeping only this header
// comment. Run `amoeboid config --help` for what each key means.
";

/// The preset new VMs are created from when the configuration names none.
pub const DEFAULT_PRESET: &str = "default";

/// The `ssh` block.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SshConfig {
    /// Host port forwarded to the guest's sshd, on 127.0.0.1; `Auto` picks a free one per start.
    pub port: SshPort,
}

impl SshConfig {
    pub const DEFAULT: Self = Self {
        port: SshPort::Auto,
    };
}

impl Default for SshConfig {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The `timeouts` block.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TimeoutsConfig {
    /// Seconds a detached start waits for the guest's sshd; `--boot-timeout` overrides.
    pub boot: Seconds,
    /// Seconds the guest gets to power off before qemu is killed; `--timeout` overrides.
    pub stop: Seconds,
}

impl TimeoutsConfig {
    pub const DEFAULT: Self = Self {
        boot: refined!(Seconds, 180),
        stop: refined!(Seconds, 60),
    };
}

impl Default for TimeoutsConfig {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Where decoding anchors relative text: relative paths are under `config_dir`, `~` is `home`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Anchor {
    pub config_dir: AbsDir,
    pub home: HomeDir,
}

/// The configuration of the invoking user: who the guest user is, and the defaults for `start`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RunConfig {
    /// Guest user name; with `uid`/`gid`, rendered into each VM's `host.nix` on every start.
    pub user: UserName,
    pub uid: Uid,
    pub gid: Gid,
    /// Preset name or directory that new VMs are created from.
    pub base_nix_config: BaseSpec,
    /// Shared when `start` is given no mounts.
    pub default_mounts: Vec<MountRequest>,
    /// Paths relative to `$HOME` copied into the guest's home on every start.
    pub home_files: Vec<HomeRel>,
    pub ssh: SshConfig,
    pub timeouts: TimeoutsConfig,
    pub linger: LingerPolicy,
    pub home_storage: HomeStorage,
}

/// The `default` preset, the base of a configuration that names none.
#[must_use]
pub fn default_base() -> BaseSpec {
    BaseSpec::Preset(refined!(PresetName, DEFAULT_PRESET))
}

impl RunConfig {
    /// A configuration for `user` with every other setting at its default.
    #[must_use]
    pub fn new(user: UserName, uid: Uid, gid: Gid) -> Self {
        Self {
            user,
            uid,
            gid,
            base_nix_config: default_base(),
            default_mounts: Vec::new(),
            home_files: Vec::new(),
            ssh: SshConfig::DEFAULT,
            timeouts: TimeoutsConfig::DEFAULT,
            linger: LingerPolicy::default(),
            home_storage: HomeStorage::default(),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_documented_ones() {
        assert_eq!(SshConfig::default().port, SshPort::Auto);
        assert_eq!(*TimeoutsConfig::default().boot.get(), 180);
        assert_eq!(*TimeoutsConfig::default().stop.get(), 60);
    }

    #[test]
    fn the_default_preset_is_a_preset_name() {
        assert_eq!(
            default_base(),
            BaseSpec::Preset(PresetName::parse("default".to_owned()).unwrap())
        );
    }

    #[test]
    fn a_new_config_has_the_defaults_and_no_mounts_or_home_files() {
        let config = RunConfig::new(
            UserName::parse("alice".to_owned()).unwrap(),
            Uid::parse(1000).unwrap(),
            Gid::parse(100).unwrap(),
        );
        assert_eq!(config.base_nix_config, default_base());
        assert_eq!(config.default_mounts, Vec::new());
        assert_eq!(config.home_files, Vec::new());
        assert_eq!(config.ssh, SshConfig::DEFAULT);
        assert_eq!(config.timeouts, TimeoutsConfig::DEFAULT);
        assert_eq!(config.linger, LingerPolicy::Enable);
        assert_eq!(config.home_storage, HomeStorage::Share);
    }
}
