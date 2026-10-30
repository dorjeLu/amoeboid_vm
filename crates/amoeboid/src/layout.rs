//! Where amoeboid keeps things (design §One lock, §Runtime dir): every location is a total
//! function of the environment values refined here, the uid and a VM name.
//!
//! [`Paths::resolve`] is the environment boundary: raw `$HOME`, `$XDG_*` and `$TMPDIR` values come
//! in as [`Var`]s and leave as [`HomeDir`], [`XdgDir`] and [`TmpDir`]. Symlinks are not resolved
//! (that would be an effect); `HostDir` observes directories where it matters.

use std::fmt;

use crate::diagnostic::Diagnostic;
use crate::domain::abs_path::{Segment, sealed};
use crate::domain::run_dir_name::{self, RunDirName};
use crate::domain::{
    AbsDir, AbsPath, AbsPathError, FileName, FlakeDir, FlakePath, FlakePathError, Fragment,
    HomeDir, Socket, TmpDir, Uid, VmName, Word, XdgDir, flake_path, vm_name, word,
};
use crate::refined;

/// Longest socket path the kernel accepts: `sun_path` is 108 bytes including the final NUL.
pub const SOCKET_PATH_MAX: usize = 107;

/// The longest socket name a runtime dir holds: `ro-store.sock` of contract version 1 (the others
/// are `control.sock`, `state.sock` and `dN.sock`, at most `d65535.sock`). Must follow the
/// contract's socket names.
pub const LONGEST_SOCKET_NAME: &str = "ro-store.sock";

/// The directory amoeboid keeps its configuration and data in, below the XDG bases.
const APP: FileName = refined!(FileName, "amoeboid");
const CONFIG_FILE: FileName = refined!(FileName, "run_config.kdl");
const PRESETS: FileName = refined!(FileName, "nix_presets");
const DOT_CONFIG: FileName = refined!(FileName, ".config");
const DOT_LOCAL: FileName = refined!(FileName, ".local");
const SHARE: FileName = refined!(FileName, "share");
const LOCK: FileName = refined!(FileName, "lock");
const RUN: FileName = refined!(FileName, "run");
const MOUNTS: FileName = refined!(FileName, "mounts");
const BUILD_LINK: FileName = refined!(FileName, "vm");
const RUNNER: FileName = refined!(FileName, "runner");
const VIRTIOFSD: FileName = refined!(FileName, "virtiofsd");
const CONTRACT: FileName = refined!(FileName, "contract");
const BIN: FileName = refined!(FileName, "bin");
const MICROVM_RUN: FileName = refined!(FileName, "microvm-run");
const GUEST: FileName = refined!(FileName, "guest");
const TRASH: FileName = refined!(FileName, ".trash");
pub(crate) const HOST_MODULE: FileName = refined!(FileName, "host.nix");
const NIX_CONFIG: FileName = refined!(FileName, "nix");
const SSH: FileName = refined!(FileName, "ssh");
const HOST_KEY: FileName = refined!(FileName, "ssh_host_ed25519_key");
const HOST_KEY_PUB: FileName = refined!(FileName, "ssh_host_ed25519_key.pub");
const KEY_STAGING: FileName = refined!(FileName, ".keygen");
const KNOWN_HOSTS: FileName = refined!(FileName, "known_hosts");
const CONSOLE_LOG: FileName = refined!(FileName, "console.log");
const QEMU_ARGS: FileName = refined!(FileName, "amoeboid-qemu-args");
const OWNER: FileName = refined!(FileName, "owner");

/// The raw value of one environment variable, as the caller read it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Var {
    Unset,
    /// Set, but not UTF-8; amoeboid's path types hold text.
    NotUnicode,
    Text(String),
}

/// The environment variables layout reads, raw.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Env {
    pub home: Var,
    pub xdg_config_home: Var,
    pub xdg_data_home: Var,
    pub xdg_runtime_dir: Var,
    pub tmpdir: Var,
}

impl Env {
    /// No variable set.
    pub const UNSET: Self = Self {
        home: Var::Unset,
        xdg_config_home: Var::Unset,
        xdg_data_home: Var::Unset,
        xdg_runtime_dir: Var::Unset,
        tmpdir: Var::Unset,
    };
}

/// The variables of [`Env`], by name.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum EnvName {
    Home,
    XdgConfigHome,
    XdgDataHome,
    XdgRuntimeDir,
    TmpDir,
}

impl EnvName {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Home => "HOME",
            Self::XdgConfigHome => "XDG_CONFIG_HOME",
            Self::XdgDataHome => "XDG_DATA_HOME",
            Self::XdgRuntimeDir => "XDG_RUNTIME_DIR",
            Self::TmpDir => "TMPDIR",
        }
    }
}

impl fmt::Display for EnvName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum LayoutError {
    #[error("$HOME is unset or empty")]
    HomeUnset,
    #[error("${var} is not valid UTF-8")]
    NotUnicode { var: EnvName },
    #[error("can't use ${var}={value:?}: {error}")]
    BadPath {
        var: EnvName,
        value: String,
        error: AbsPathError,
    },
    #[error(
        "can't place sockets under {root}: the longest socket path would be {len} bytes, \
         more than {max}"
    )]
    SocketPathTooLong {
        root: AbsDir,
        len: usize,
        max: usize,
    },
    /// The data dir ends up in `path:` flake URLs for `nix build`.
    #[error("can't use {path} as the data dir: {error}")]
    BadDataDir { path: AbsDir, error: FlakePathError },
}

impl Diagnostic for LayoutError {
    fn code(&self) -> &'static str {
        match self {
            Self::HomeUnset => "layout.home-unset",
            Self::NotUnicode { .. } => "layout.not-unicode",
            Self::BadPath { .. } => "layout.bad-path",
            Self::SocketPathTooLong { .. } => "layout.socket-path-too-long",
            Self::BadDataDir { .. } => "layout.bad-data-dir",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::HomeUnset
            | Self::NotUnicode {
                var: EnvName::Home, ..
            }
            | Self::BadPath {
                var: EnvName::Home, ..
            } => "run amoeboid from a login session, with $HOME an absolute path".to_owned(),
            Self::NotUnicode { var } | Self::BadPath { var, .. } => {
                format!("set ${var} to an absolute UTF-8 path without '..', or unset it")
            }
            Self::SocketPathTooLong { .. } => {
                "set $XDG_RUNTIME_DIR (or $TMPDIR) to a shorter directory".to_owned()
            }
            Self::BadDataDir { .. } => format!(
                "set $XDG_DATA_HOME to a directory whose path uses only letters, digits, '/' and \
                 any of {}",
                flake_path::ALLOWED
            ),
        })
    }
}

/// A variable's value, sorted by what the XDG rules make of it.
enum Raw<'a> {
    /// Unset or empty.
    Absent,
    Relative(&'a str),
    Absolute(&'a str),
}

fn classify(var: EnvName, value: &Var) -> Result<Raw<'_>, LayoutError> {
    match value {
        Var::Unset => Ok(Raw::Absent),
        Var::NotUnicode => Err(LayoutError::NotUnicode { var }),
        Var::Text(text) if text.is_empty() => Ok(Raw::Absent),
        Var::Text(text) if text.starts_with('/') => Ok(Raw::Absolute(text)),
        Var::Text(text) => Ok(Raw::Relative(text)),
    }
}

fn absolute<K>(var: EnvName, text: &str) -> Result<crate::domain::abs_path::Abs<K>, LayoutError> {
    crate::domain::abs_path::Abs::normalising(text).map_err(|error| LayoutError::BadPath {
        var,
        value: text.to_owned(),
        error,
    })
}

/// `$HOME`: required and absolute; empty and `.` components are dropped.
///
/// # Errors
/// Unset or empty, not UTF-8, relative, or with a `..` or NUL in it.
pub fn home_dir(value: &Var) -> Result<HomeDir, LayoutError> {
    let var = EnvName::Home;
    match classify(var, value)? {
        Raw::Absent => Err(LayoutError::HomeUnset),
        Raw::Relative(text) => Err(LayoutError::BadPath {
            var,
            value: text.to_owned(),
            error: AbsPathError::NotAbsolute,
        }),
        Raw::Absolute(text) => absolute(var, text),
    }
}

/// An `$XDG_*` base directory; `None` when unset, empty or relative (the XDG spec says relative
/// values are invalid and must be ignored).
///
/// # Errors
/// Not UTF-8, or absolute with a `..` or NUL in it.
pub fn xdg_dir(var: EnvName, value: &Var) -> Result<Option<XdgDir>, LayoutError> {
    match classify(var, value)? {
        Raw::Absent | Raw::Relative(_) => Ok(None),
        Raw::Absolute(text) => absolute(var, text).map(Some),
    }
}

/// `$TMPDIR`, by the same rules as the XDG variables.
///
/// # Errors
/// As [`xdg_dir`].
pub fn tmp_dir(value: &Var) -> Result<Option<TmpDir>, LayoutError> {
    match classify(EnvName::TmpDir, value)? {
        Raw::Absent | Raw::Relative(_) => Ok(None),
        Raw::Absolute(text) => absolute(EnvName::TmpDir, text).map(Some),
    }
}

/// Prefix of the per-user directory under the temp dir; a [`Word`], so the name is one component.
const USER_TMP_PREFIX: Word = refined!(Word, "amoeboid-");

/// `amoeboid-<uid>`: the per-user directory under the temp dir.
struct UserTmp(String);

impl UserTmp {
    fn of(uid: Uid) -> Self {
        Self(word::concat(&[
            Fragment::Text(USER_TMP_PREFIX),
            Fragment::Uid(uid),
        ]))
    }
}

/// A word and decimal digits are one component.
impl sealed::Sealed for UserTmp {}

impl Segment for UserTmp {
    fn segment(&self) -> &str {
        &self.0
    }
}

/// Every location of the invoking user, from the environment.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Paths {
    home: HomeDir,
    config_dir: AbsDir,
    data_dir: FlakeDir,
    runtime_root: AbsDir,
}

impl Paths {
    /// Config under `$XDG_CONFIG_HOME` (else `~/.config`), data under `$XDG_DATA_HOME` (else
    /// `~/.local/share`), each in `amoeboid/`; runtime dirs directly in `$XDG_RUNTIME_DIR`, else in
    /// `$TMPDIR/amoeboid-<uid>` (else `/tmp/amoeboid-<uid>`).
    ///
    /// # Errors
    /// The first bad variable (in [`EnvName`] order), a runtime root so long that the longest
    /// socket path of the longest VM name would not fit `sun_path`, or a data dir with a character
    /// Nix misreads or refuses in a `path:` flake URL (see [`FlakePathError`]).
    pub fn resolve(env: &Env, uid: Uid) -> Result<Self, LayoutError> {
        let home = home_dir(&env.home)?;
        let config_base = xdg_dir(EnvName::XdgConfigHome, &env.xdg_config_home)?
            .map_or_else(|| home.join_dir(&DOT_CONFIG), |dir| dir.to_dir());
        let data_base = xdg_dir(EnvName::XdgDataHome, &env.xdg_data_home)?.map_or_else(
            || home.join_dir(&DOT_LOCAL).join_dir(&SHARE),
            |dir| dir.to_dir(),
        );
        let runtime = xdg_dir(EnvName::XdgRuntimeDir, &env.xdg_runtime_dir)?;
        let tmp = tmp_dir(&env.tmpdir)?;
        let runtime_root = match (runtime, tmp) {
            (Some(dir), _) => dir.to_dir(),
            (None, Some(tmp)) => tmp.join_dir(&UserTmp::of(uid)),
            (None, None) => AbsDir::root()
                .join_dir(&refined!(FileName, "tmp"))
                .join_dir(&UserTmp::of(uid)),
        };
        let len = longest_socket_path(&runtime_root);
        if len > SOCKET_PATH_MAX {
            return Err(LayoutError::SocketPathTooLong {
                root: runtime_root,
                len,
                max: SOCKET_PATH_MAX,
            });
        }
        let data_dir = data_base.join_dir(&APP);
        let data_dir =
            FlakeDir::parse(data_dir.clone()).map_err(|error| LayoutError::BadDataDir {
                path: data_dir,
                error,
            })?;
        Ok(Self {
            config_dir: config_base.join_dir(&APP),
            data_dir,
            home,
            runtime_root,
        })
    }

    #[must_use]
    pub fn home(&self) -> &HomeDir {
        &self.home
    }

    /// `amoeboid/` under the XDG config base.
    #[must_use]
    pub fn config_dir(&self) -> &AbsDir {
        &self.config_dir
    }

    /// `amoeboid/` under the XDG data base; holds one directory per VM.
    #[must_use]
    pub fn data_dir(&self) -> &AbsDir {
        self.data_dir.abs()
    }

    /// Holds one runtime dir per VM.
    #[must_use]
    pub fn runtime_root(&self) -> &AbsDir {
        &self.runtime_root
    }

    /// Where the Python launcher kept VM `name`'s runtime files (`<root>/amoeboid/NAME`), to
    /// notice a VM it left running.
    #[must_use]
    pub fn legacy_runtime(&self, name: &VmName) -> AbsDir {
        self.runtime_root.join_dir(&APP).join_dir(name)
    }

    #[must_use]
    pub fn config_file(&self) -> AbsPath {
        self.config_dir.join(&CONFIG_FILE)
    }

    #[must_use]
    pub fn presets_dir(&self) -> AbsDir {
        self.config_dir.join_dir(&PRESETS)
    }

    /// The locations of VM `name`.
    #[must_use]
    pub fn vm(&self, name: &VmName) -> VmPaths {
        VmPaths {
            state: self.data_dir.join_dir(name),
            runtime: RuntimeDir(self.runtime_root.join_dir(&RunDirName::of(name))),
            name: name.clone(),
        }
    }
}

/// Bytes of `<root>/amb_<40 chars>/<longest socket name>`.
fn longest_socket_path(root: &AbsDir) -> usize {
    root.get()
        .len()
        .saturating_add(1)
        .saturating_add(run_dir_name::PREFIX.get().len())
        .saturating_add(vm_name::MAX_LEN)
        .saturating_add(1)
        .saturating_add(LONGEST_SOCKET_NAME.len())
}

/// What the flake's build provides, as the launcher uses it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum BuildFile {
    /// The VM runner script.
    Runner,
    Virtiofsd,
    /// `contract.json`.
    Contract,
}

/// The files of one VM: persistent state under the data dir, ephemeral ones in its runtime dir.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct VmPaths {
    name: VmName,
    state: FlakeDir,
    runtime: RuntimeDir,
}

impl VmPaths {
    #[must_use]
    pub fn name(&self) -> &VmName {
        &self.name
    }

    /// `<data>/NAME`: survives VM restarts and host reboots.
    #[must_use]
    pub fn state(&self) -> &AbsDir {
        self.state.abs()
    }

    #[must_use]
    pub fn runtime(&self) -> &RuntimeDir {
        &self.runtime
    }

    /// The VM lock; created on demand, never deleted.
    #[must_use]
    pub fn lock(&self) -> AbsPath {
        self.state.abs().join(&LOCK)
    }

    /// The run record of the running VM.
    #[must_use]
    pub fn run(&self) -> AbsPath {
        self.state.abs().join(&RUN)
    }

    /// What the last start shared; outside the guest's reach, unlike `guest/mounts`.
    #[must_use]
    pub fn last_mounts(&self) -> AbsPath {
        self.state.abs().join(&MOUNTS)
    }

    /// Out-link of the VM's build.
    #[must_use]
    pub fn build_link(&self) -> AbsPath {
        self.state.abs().join(&BUILD_LINK)
    }

    /// A file of the build, below the out-link. The names are the flake's linkFarm entries
    /// (`runner`, `virtiofsd`, `contract`) and the programs microvm.nix and nixpkgs put in them.
    #[must_use]
    pub fn build_file(&self, file: BuildFile) -> AbsPath {
        let link = self.state.abs().join_dir(&BUILD_LINK);
        match file {
            BuildFile::Runner => link.join_dir(&RUNNER).join_dir(&BIN).join(&MICROVM_RUN),
            BuildFile::Virtiofsd => link.join_dir(&VIRTIOFSD).join_dir(&BIN).join(&VIRTIOFSD),
            BuildFile::Contract => link.join(&CONTRACT),
        }
    }

    /// Shared into the VM at `/mnt/amoeboid`; the guest can write anything in here.
    #[must_use]
    pub fn guest(&self) -> AbsDir {
        self.state.abs().join_dir(&GUEST)
    }

    /// Where a replaced guest tree goes to be deleted; host-owned and on the filesystem of
    /// [`VmPaths::guest`], so moving a tree there is a rename.
    #[must_use]
    pub fn trash(&self) -> AbsDir {
        self.state.abs().join_dir(&TRASH)
    }

    /// Guest account from `run_config.kdl`, rendered every start; the flake's `host` input. A
    /// flake path: `nix build` gets it in a `path:` URL.
    #[must_use]
    pub fn host_module(&self) -> FlakePath {
        self.state.join(&HOST_MODULE)
    }

    /// This VM's own copy of its NixOS configuration (host-owned); what gets built, so a flake
    /// path.
    #[must_use]
    pub fn nix_config(&self) -> FlakeDir {
        self.state.join_dir(&NIX_CONFIG)
    }

    #[must_use]
    pub fn ssh_dir(&self) -> AbsDir {
        self.state.abs().join_dir(&SSH)
    }

    #[must_use]
    pub fn host_key(&self) -> AbsPath {
        self.ssh_dir().join(&HOST_KEY)
    }

    /// The public half, beside [`VmPaths::host_key`] (`ssh-keygen` names it so).
    #[must_use]
    pub fn host_key_pub(&self) -> AbsPath {
        self.ssh_dir().join(&HOST_KEY_PUB)
    }

    /// Where a new host key is generated before its files are renamed into place; holds nothing
    /// of value (whatever is found there is a leftover and is cleared).
    #[must_use]
    pub fn host_key_staging(&self) -> AbsDir {
        self.ssh_dir().join_dir(&KEY_STAGING)
    }

    /// [`VmPaths::host_key`] while it is being generated.
    #[must_use]
    pub fn staged_host_key(&self) -> AbsPath {
        self.host_key_staging().join(&HOST_KEY)
    }

    /// [`VmPaths::host_key_pub`] while it is being generated.
    #[must_use]
    pub fn staged_host_key_pub(&self) -> AbsPath {
        self.host_key_staging().join(&HOST_KEY_PUB)
    }

    #[must_use]
    pub fn known_hosts(&self) -> AbsPath {
        self.ssh_dir().join(&KNOWN_HOSTS)
    }

    #[must_use]
    pub fn console_log(&self) -> AbsPath {
        self.state.abs().join(&CONSOLE_LOG)
    }
}

/// `<runtime root>/amb_NAME`: sockets, the qemu argument file and the `owner` file. Only
/// [`Paths::vm`] makes one, so every socket name up to [`LONGEST_SOCKET_NAME`] fits below it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RuntimeDir(AbsDir);

impl RuntimeDir {
    #[must_use]
    pub fn path(&self) -> &AbsDir {
        &self.0
    }

    /// Extra qemu arguments; the runner word-splits it.
    #[must_use]
    pub fn qemu_args(&self) -> AbsPath {
        self.0.join(&QEMU_ARGS)
    }

    /// Names the VM the directory belongs to.
    #[must_use]
    pub fn owner(&self) -> AbsPath {
        self.0.join(&OWNER)
    }

    /// Where `socket` lives. Total: [`Paths::resolve`] left room for the longest socket name, and
    /// every [`Socket`]'s name is at most that long.
    #[must_use]
    pub fn socket(&self, socket: Socket) -> SocketPath {
        SocketPath(self.0.join(&socket.name()))
    }
}

/// An absolute path of at most [`SOCKET_PATH_MAX`] bytes, so it fits `sun_path`.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct SocketPath(AbsPath);

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum SocketPathError {
    #[error("socket path {path} is {len} bytes, more than {SOCKET_PATH_MAX}")]
    TooLong { path: AbsPath, len: usize },
}

impl Diagnostic for SocketPathError {
    fn code(&self) -> &'static str {
        match self {
            Self::TooLong { .. } => "socket-path.too-long",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::TooLong { .. } => {
                Some("set $XDG_RUNTIME_DIR (or $TMPDIR) to a shorter directory".to_owned())
            }
        }
    }
}

impl SocketPath {
    /// # Errors
    /// `TooLong` above [`SOCKET_PATH_MAX`] bytes.
    pub fn parse(path: AbsPath) -> Result<Self, SocketPathError> {
        let len = path.get().len();
        if len > SOCKET_PATH_MAX {
            return Err(SocketPathError::TooLong { path, len });
        }
        Ok(Self(path))
    }

    #[must_use]
    pub fn get(&self) -> &AbsPath {
        &self.0
    }
}

impl fmt::Display for SocketPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::domain::abs_path::tests::is_segment;
    use crate::testing::{self, Entry};

    fn witness(e: &LayoutError) -> &'static str {
        match e {
            LayoutError::HomeUnset => "HomeUnset",
            LayoutError::NotUnicode { .. } => "NotUnicode",
            LayoutError::BadPath { .. } => "BadPath",
            LayoutError::SocketPathTooLong { .. } => "SocketPathTooLong",
            LayoutError::BadDataDir { .. } => "BadDataDir",
        }
    }

    fn errors() -> Vec<LayoutError> {
        vec![
            LayoutError::HomeUnset,
            LayoutError::NotUnicode {
                var: EnvName::TmpDir,
            },
            LayoutError::BadPath {
                var: EnvName::XdgDataHome,
                value: "/a/../b".to_owned(),
                error: AbsPathError::ParentDir { index: 1 },
            },
            LayoutError::SocketPathTooLong {
                root: dir("/tmp/long"),
                len: 108,
                max: SOCKET_PATH_MAX,
            },
            LayoutError::BadDataDir {
                path: dir("/srv/a?b/amoeboid"),
                error: FlakePathError::Query { at: 6 },
            },
        ]
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        errors().iter().map(Entry::of).collect()
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(
            &errors(),
            witness,
            &[
                "HomeUnset",
                "NotUnicode",
                "BadPath",
                "SocketPathTooLong",
                "BadDataDir",
            ],
        );
    }

    #[test]
    fn home_errors_hint_at_a_login_session_and_others_at_unsetting_the_variable() {
        let login =
            Some("run amoeboid from a login session, with $HOME an absolute path".to_owned());
        testing::table(
            &[
                (LayoutError::HomeUnset, login.clone()),
                (
                    LayoutError::NotUnicode { var: EnvName::Home },
                    login.clone(),
                ),
                (
                    LayoutError::BadPath {
                        var: EnvName::Home,
                        value: "x".to_owned(),
                        error: AbsPathError::NotAbsolute,
                    },
                    login,
                ),
                (
                    LayoutError::NotUnicode {
                        var: EnvName::XdgConfigHome,
                    },
                    Some(
                        "set $XDG_CONFIG_HOME to an absolute UTF-8 path without '..', or unset it"
                            .to_owned(),
                    ),
                ),
            ],
            Diagnostic::hint,
        );
    }

    #[test]
    fn a_data_dir_error_hints_at_xdg_data_home() {
        let error = LayoutError::BadDataDir {
            path: dir("/h#/.local/share/amoeboid"),
            error: FlakePathError::Fragment { at: 2 },
        };
        assert_eq!(
            error.hint(),
            Some(
                "set $XDG_DATA_HOME to a directory whose path uses only letters, digits, '/' \
                 and any of !$&'()*+,-.:;=@_~"
                    .to_owned()
            )
        );
    }

    #[test]
    fn variables_are_named_as_in_the_environment() {
        testing::table(
            &[
                (EnvName::Home, "HOME"),
                (EnvName::XdgConfigHome, "XDG_CONFIG_HOME"),
                (EnvName::XdgDataHome, "XDG_DATA_HOME"),
                (EnvName::XdgRuntimeDir, "XDG_RUNTIME_DIR"),
                (EnvName::TmpDir, "TMPDIR"),
            ],
            |name| {
                assert_eq!(name.to_string(), name.as_str());
                name.as_str()
            },
        );
    }

    fn dir(text: &str) -> AbsDir {
        AbsDir::parse(text.to_owned()).unwrap()
    }

    fn text(value: &str) -> Var {
        Var::Text(value.to_owned())
    }

    fn uid(n: u32) -> Uid {
        Uid::parse(n).unwrap()
    }

    fn vm(name: &str) -> VmName {
        VmName::parse(name.to_owned()).unwrap()
    }

    fn paths(env: &Env) -> Paths {
        Paths::resolve(env, uid(4242)).unwrap()
    }

    fn bad(var: EnvName, value: &str, error: AbsPathError) -> LayoutError {
        LayoutError::BadPath {
            var,
            value: value.to_owned(),
            error,
        }
    }

    #[test]
    fn home_dir_rejection_table() {
        testing::table(
            &[
                (Var::Unset, Err(LayoutError::HomeUnset)),
                (text(""), Err(LayoutError::HomeUnset)),
                (
                    Var::NotUnicode,
                    Err(LayoutError::NotUnicode { var: EnvName::Home }),
                ),
                (
                    text("relative/home"),
                    Err(bad(
                        EnvName::Home,
                        "relative/home",
                        AbsPathError::NotAbsolute,
                    )),
                ),
                (
                    text("~/x"),
                    Err(bad(EnvName::Home, "~/x", AbsPathError::NotAbsolute)),
                ),
                (
                    text("/home/../root"),
                    Err(bad(
                        EnvName::Home,
                        "/home/../root",
                        AbsPathError::ParentDir { index: 1 },
                    )),
                ),
                (
                    text("/home/u\0"),
                    Err(bad(
                        EnvName::Home,
                        "/home/u\0",
                        AbsPathError::NulByte { at: 7 },
                    )),
                ),
                (text("/home/u"), Ok("/home/u".to_owned())),
                (text("/home/u/"), Ok("/home/u".to_owned())),
                (text("//home/./u"), Ok("/home/u".to_owned())),
                (text("/"), Ok("/".to_owned())),
            ],
            |value| home_dir(value).map(HomeDir::into_inner),
        );
    }

    #[test]
    fn xdg_dir_rejection_table() {
        let var = EnvName::XdgDataHome;
        testing::table(
            &[
                (Var::Unset, Ok(None)),
                (text(""), Ok(None)),
                (text("relative/data"), Ok(None)),
                (text("./data"), Ok(None)),
                (Var::NotUnicode, Err(LayoutError::NotUnicode { var })),
                (
                    text("/a/.."),
                    Err(bad(var, "/a/..", AbsPathError::ParentDir { index: 1 })),
                ),
                (
                    text("/a\0"),
                    Err(bad(var, "/a\0", AbsPathError::NulByte { at: 2 })),
                ),
                (text("/srv/data/"), Ok(Some("/srv/data".to_owned()))),
            ],
            |value| xdg_dir(var, value).map(|dir| dir.map(XdgDir::into_inner)),
        );
    }

    #[test]
    fn tmp_dir_rejection_table() {
        let var = EnvName::TmpDir;
        testing::table(
            &[
                (Var::Unset, Ok(None)),
                (text(""), Ok(None)),
                (text("tmp"), Ok(None)),
                (Var::NotUnicode, Err(LayoutError::NotUnicode { var })),
                (
                    text("/../tmp"),
                    Err(bad(var, "/../tmp", AbsPathError::ParentDir { index: 0 })),
                ),
                (text("/var/tmp//"), Ok(Some("/var/tmp".to_owned()))),
            ],
            |value| tmp_dir(value).map(|dir| dir.map(TmpDir::into_inner)),
        );
    }

    fn home(value: &str) -> Env {
        Env {
            home: text(value),
            ..Env::UNSET
        }
    }

    #[test]
    fn defaults_derive_from_home() {
        let p = paths(&home("/home/u"));
        assert_eq!(p.home().get(), "/home/u");
        assert_eq!(p.config_dir(), &dir("/home/u/.config/amoeboid"));
        assert_eq!(
            p.config_file().get(),
            "/home/u/.config/amoeboid/run_config.kdl"
        );
        assert_eq!(p.presets_dir(), dir("/home/u/.config/amoeboid/nix_presets"));
        assert_eq!(p.data_dir(), &dir("/home/u/.local/share/amoeboid"));
        assert_eq!(p.runtime_root(), &dir("/tmp/amoeboid-4242"));
    }

    #[test]
    fn the_python_runtime_dir_is_amoeboid_then_the_vm_name_under_the_root() {
        let name = VmName::parse("proj".to_owned()).unwrap();
        assert_eq!(
            paths(&home("/home/u")).legacy_runtime(&name),
            dir("/tmp/amoeboid-4242/amoeboid/proj")
        );
        let env = Env {
            xdg_runtime_dir: text("/run/user/7"),
            ..home("/h")
        };
        assert_eq!(
            paths(&env).legacy_runtime(&name),
            dir("/run/user/7/amoeboid/proj")
        );
    }

    #[test]
    fn xdg_variables_win_and_relative_ones_are_ignored() {
        let env = Env {
            xdg_config_home: text("/h/cfg"),
            xdg_data_home: text("relative/data"),
            xdg_runtime_dir: text("/run/user/7"),
            ..home("/h")
        };
        let p = paths(&env);
        assert_eq!(p.config_dir(), &dir("/h/cfg/amoeboid"));
        assert_eq!(p.data_dir(), &dir("/h/.local/share/amoeboid"));
        assert_eq!(p.runtime_root(), &dir("/run/user/7"));
    }

    #[test]
    fn an_absolute_xdg_data_home_holds_the_data_dir() {
        let env = Env {
            xdg_data_home: text("/srv/data/"),
            ..home("/h")
        };
        assert_eq!(paths(&env).data_dir(), &dir("/srv/data/amoeboid"));
    }

    #[test]
    fn tmpdir_is_the_runtime_fallback() {
        let env = Env {
            tmpdir: text("/h/t"),
            ..home("/h")
        };
        assert_eq!(paths(&env).runtime_root(), &dir("/h/t/amoeboid-4242"));
        let ignored = Env {
            tmpdir: text("t"),
            ..home("/h")
        };
        assert_eq!(paths(&ignored).runtime_root(), &dir("/tmp/amoeboid-4242"));
    }

    #[test]
    fn home_must_be_absolute() {
        for value in [Var::Unset, text(""), text("relative/home")] {
            let env = Env {
                home: value.clone(),
                ..Env::UNSET
            };
            assert!(Paths::resolve(&env, uid(1)).is_err(), "{value:?}");
        }
    }

    #[test]
    fn the_first_bad_variable_is_reported() {
        let env = Env {
            xdg_config_home: Var::NotUnicode,
            xdg_data_home: Var::NotUnicode,
            ..home("/h")
        };
        assert_eq!(
            Paths::resolve(&env, uid(1)),
            Err(LayoutError::NotUnicode {
                var: EnvName::XdgConfigHome
            })
        );
        for (env, var) in [
            (
                Env {
                    xdg_data_home: text("/.."),
                    ..home("/h")
                },
                EnvName::XdgDataHome,
            ),
            (
                Env {
                    xdg_runtime_dir: text("/.."),
                    ..home("/h")
                },
                EnvName::XdgRuntimeDir,
            ),
            (
                Env {
                    tmpdir: text("/.."),
                    ..home("/h")
                },
                EnvName::TmpDir,
            ),
        ] {
            assert_eq!(
                Paths::resolve(&env, uid(1)),
                Err(bad(var, "/..", AbsPathError::ParentDir { index: 0 }))
            );
        }
    }

    #[test]
    fn a_data_dir_that_nix_would_misread_in_a_path_url_is_refused_at_resolve() {
        let data = |base: &str| Env {
            xdg_data_home: text(base),
            ..home("/h")
        };
        let refused = |path: &str, error| {
            Err(LayoutError::BadDataDir {
                path: dir(path),
                error,
            })
        };
        testing::table(
            &[
                (
                    data("/srv/a?b"),
                    refused("/srv/a?b/amoeboid", FlakePathError::Query { at: 6 }),
                ),
                (
                    data("/srv/a#b"),
                    refused("/srv/a#b/amoeboid", FlakePathError::Fragment { at: 6 }),
                ),
                (
                    data("/srv/%41"),
                    refused("/srv/%41/amoeboid", FlakePathError::Percent { at: 5 }),
                ),
                (
                    // $HOME is the data dir's base when no XDG variable says otherwise.
                    home("/home/u?"),
                    refused(
                        "/home/u?/.local/share/amoeboid",
                        FlakePathError::Query { at: 7 },
                    ),
                ),
                (
                    data("/srv/a b"),
                    refused(
                        "/srv/a b/amoeboid",
                        FlakePathError::BadChar { ch: ' ', at: 6 },
                    ),
                ),
                (
                    data("/srv/a:c&d=e+f"),
                    Ok("/srv/a:c&d=e+f/amoeboid".to_owned()),
                ),
                // Only the data dir is a flake path: the others may hold these characters.
                (
                    Env {
                        xdg_config_home: text("/c?#%"),
                        xdg_runtime_dir: text("/r?#%"),
                        ..home("/h")
                    },
                    Ok("/h/.local/share/amoeboid".to_owned()),
                ),
            ],
            |env| Paths::resolve(env, uid(1)).map(|p| p.data_dir().get().clone()),
        );
    }

    #[test]
    fn vm_paths() {
        let env = Env {
            xdg_runtime_dir: text("/run/user/7"),
            ..home("/h")
        };
        let box_ = paths(&env).vm(&vm("box"));
        let state = "/h/.local/share/amoeboid/box";
        testing::table(
            &[
                ("state", state.to_owned()),
                ("lock", format!("{state}/lock")),
                ("run", format!("{state}/run")),
                ("last_mounts", format!("{state}/mounts")),
                ("build_link", format!("{state}/vm")),
                ("runner", format!("{state}/vm/runner/bin/microvm-run")),
                ("virtiofsd", format!("{state}/vm/virtiofsd/bin/virtiofsd")),
                ("contract", format!("{state}/vm/contract")),
                ("guest", format!("{state}/guest")),
                ("trash", format!("{state}/.trash")),
                ("host_module", format!("{state}/host.nix")),
                ("nix_config", format!("{state}/nix")),
                ("ssh_dir", format!("{state}/ssh")),
                ("host_key", format!("{state}/ssh/ssh_host_ed25519_key")),
                (
                    "host_key_pub",
                    format!("{state}/ssh/ssh_host_ed25519_key.pub"),
                ),
                ("host_key_staging", format!("{state}/ssh/.keygen")),
                (
                    "staged_host_key",
                    format!("{state}/ssh/.keygen/ssh_host_ed25519_key"),
                ),
                (
                    "staged_host_key_pub",
                    format!("{state}/ssh/.keygen/ssh_host_ed25519_key.pub"),
                ),
                ("known_hosts", format!("{state}/ssh/known_hosts")),
                ("console_log", format!("{state}/console.log")),
                ("runtime", "/run/user/7/amb_box".to_owned()),
                (
                    "qemu_args",
                    "/run/user/7/amb_box/amoeboid-qemu-args".to_owned(),
                ),
                ("owner", "/run/user/7/amb_box/owner".to_owned()),
            ],
            |which| {
                let path = match *which {
                    "state" => box_.state().get().clone(),
                    "lock" => box_.lock().into_inner(),
                    "run" => box_.run().into_inner(),
                    "last_mounts" => box_.last_mounts().into_inner(),
                    "build_link" => box_.build_link().into_inner(),
                    "runner" => box_.build_file(BuildFile::Runner).into_inner(),
                    "virtiofsd" => box_.build_file(BuildFile::Virtiofsd).into_inner(),
                    "contract" => box_.build_file(BuildFile::Contract).into_inner(),
                    "guest" => box_.guest().into_inner(),
                    "trash" => box_.trash().into_inner(),
                    "host_module" => box_.host_module().abs().get().clone(),
                    "nix_config" => box_.nix_config().abs().get().clone(),
                    "ssh_dir" => box_.ssh_dir().into_inner(),
                    "host_key" => box_.host_key().into_inner(),
                    "host_key_pub" => box_.host_key_pub().into_inner(),
                    "host_key_staging" => box_.host_key_staging().into_inner(),
                    "staged_host_key" => box_.staged_host_key().into_inner(),
                    "staged_host_key_pub" => box_.staged_host_key_pub().into_inner(),
                    "known_hosts" => box_.known_hosts().into_inner(),
                    "console_log" => box_.console_log().into_inner(),
                    "runtime" => box_.runtime().path().get().clone(),
                    "qemu_args" => box_.runtime().qemu_args().into_inner(),
                    "owner" => box_.runtime().owner().into_inner(),
                    other => format!("unknown {other}"),
                };
                // Every joined path is what parsing its text gives.
                assert_eq!(
                    AbsPath::parse(path.clone()).map(AbsPath::into_inner),
                    Ok(path.clone())
                );
                path
            },
        );
        assert_eq!(box_.name(), &vm("box"));
    }

    /// A runtime root of exactly `len` bytes: `/` and `x`s.
    fn root_of(len: usize) -> String {
        format!("/{}", "x".repeat(len.saturating_sub(1)))
    }

    fn too_long(root: &str, len: usize) -> Result<String, LayoutError> {
        Err(LayoutError::SocketPathTooLong {
            root: dir(root),
            len,
            max: SOCKET_PATH_MAX,
        })
    }

    #[test]
    fn a_runtime_root_is_refused_when_the_longest_socket_path_would_not_fit() {
        // 107 = root + "/amb_" (5) + 40 + "/" (1) + "ro-store.sock" (13): the root may have 48 bytes.
        let (r1, r47, r48, r49, r300) = (
            root_of(1),
            root_of(47),
            root_of(48),
            root_of(49),
            root_of(300),
        );
        testing::table(
            &[
                (r1.clone(), Ok(r1)),
                (r47.clone(), Ok(r47)),
                (r48.clone(), Ok(r48)),
                (r49.clone(), too_long(&r49, 108)),
                (r300.clone(), too_long(&r300, 359)),
            ],
            |root| {
                let env = Env {
                    xdg_runtime_dir: text(root),
                    ..home("/h")
                };
                Paths::resolve(&env, uid(1)).map(|p| p.runtime_root().get().clone())
            },
        );
    }

    #[test]
    fn a_long_tmpdir_fallback_is_refused_the_same_way() {
        // `/amoeboid-4294967294` adds 20 bytes, so `$TMPDIR` may have 28.
        let (t28, t29) = (root_of(28), root_of(29));
        let (r28, r29) = (
            format!("{t28}/amoeboid-4294967294"),
            format!("{t29}/amoeboid-4294967294"),
        );
        testing::table(&[(t28, Ok(r28)), (t29, too_long(&r29, 108))], |tmp| {
            let env = Env {
                tmpdir: text(tmp),
                ..home("/h")
            };
            Paths::resolve(&env, uid(4_294_967_294)).map(|p| p.runtime_root().get().clone())
        });
    }

    #[test]
    fn the_longest_socket_path_of_the_longest_vm_name_fits_exactly_at_the_limit() {
        let env = Env {
            xdg_runtime_dir: text(&root_of(48)),
            ..home("/h")
        };
        let longest = vm(&"z".repeat(vm_name::MAX_LEN));
        let runtime = paths(&env).vm(&longest).runtime().path().get().clone();
        assert_eq!(
            format!("{runtime}/{LONGEST_SOCKET_NAME}").len(),
            SOCKET_PATH_MAX
        );
    }

    #[test]
    fn the_default_runtime_root_of_the_largest_uid_fits() {
        let p = Paths::resolve(&home("/h"), uid(4_294_967_294)).unwrap();
        assert_eq!(p.runtime_root(), &dir("/tmp/amoeboid-4294967294"));
    }

    fn socket_witness(e: &SocketPathError) -> &'static str {
        match e {
            SocketPathError::TooLong { .. } => "TooLong",
        }
    }

    fn socket_errors() -> Vec<SocketPathError> {
        vec![SocketPathError::TooLong {
            path: AbsPath::parse(root_of(108)).unwrap(),
            len: 108,
        }]
    }

    /// One catalogue entry per variant of [`SocketPathError`].
    pub(crate) fn socket_entries() -> Vec<Entry> {
        socket_errors().iter().map(Entry::of).collect()
    }

    #[test]
    fn every_socket_path_error_variant_has_a_table_case() {
        testing::every_variant(&socket_errors(), socket_witness, &["TooLong"]);
    }

    #[test]
    fn socket_path_parse_accepts_up_to_107_bytes() {
        let (p106, p107, p108) = (root_of(106), root_of(107), root_of(108));
        testing::table(
            &[
                ("/".to_owned(), Ok("/".to_owned())),
                (p106.clone(), Ok(p106)),
                (p107.clone(), Ok(p107)),
                (
                    p108.clone(),
                    Err(SocketPathError::TooLong {
                        path: AbsPath::parse(p108).unwrap(),
                        len: 108,
                    }),
                ),
            ],
            |text| {
                SocketPath::parse(AbsPath::parse(text.clone()).unwrap())
                    .map(|path| path.get().get().clone())
            },
        );
    }

    #[test]
    fn every_socket_fits_below_the_longest_runtime_dir() {
        let env = Env {
            xdg_runtime_dir: text(&root_of(48)),
            ..home("/h")
        };
        let longest = paths(&env).vm(&vm(&"z".repeat(vm_name::MAX_LEN)));
        let names = crate::domain::socket::tests::every_socket();
        for socket in names.iter().copied() {
            let path = longest.runtime().socket(socket);
            assert_eq!(
                SocketPath::parse(path.get().clone()),
                Ok(path.clone()),
                "{socket:?}"
            );
            assert!(socket.name().as_str().len() <= LONGEST_SOCKET_NAME.len());
            assert_eq!(path.to_string(), path.get().get().as_str());
        }
        assert_eq!(Socket::Store.name().as_str(), LONGEST_SOCKET_NAME);
        assert_eq!(
            longest.runtime().socket(Socket::Store).get().get().len(),
            SOCKET_PATH_MAX
        );
    }

    #[test]
    fn the_per_user_temp_dir_name_is_one_component() {
        for n in [1, 1000, 4_294_967_294] {
            assert!(is_segment(UserTmp::of(uid(n)).segment()), "{n}");
        }
    }
}
