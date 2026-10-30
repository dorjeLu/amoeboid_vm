//! Pure construction of everything the launcher writes or runs (design §Workspace `plan/`): argv
//! for nix, virtiofsd and ssh, the qemu argument file, the guest's mounts table and `host.nix`.
//! Built only from refined values; no effects.
//!
//! An [`Argv`] or [`NixString`] cannot be made from raw text outside these functions:
//!
//! ```compile_fail,E0451
//! let argv = amoeboid::plan::Argv { program: "sh".to_owned(), args: vec![] };
//! ```
//!
//! ```compile_fail,E0603
//! let unescaped = amoeboid::plan::NixString("\"; evil = \"".to_owned());
//! ```

pub mod argv;
pub mod guest_files;
pub mod host_nix;
pub mod known_hosts;
pub mod qemu;
pub mod qmp;
pub mod share;

pub use argv::{
    Argv, IdMap, SshPassthrough, loginctl_argv, nix_build_argv, runner_argv, ssh_argv,
    ssh_keygen_argv, virtiofsd_argv,
};
pub use guest_files::{GUEST_README, GuestCopies, GuestTree, guest_copies};
pub use host_nix::{HostModule, NixString, host_module};
pub use known_hosts::{HostAlias, KnownHosts, known_hosts};
pub use qemu::{GUEST_MAC, GUEST_SSH_PORT, QemuArg, QemuArgError, QemuArgs, qemu_args};
pub use qmp::{
    GreetingError, QmpCommand, QmpDesc, QmpErrorClass, QmpGreeting, QmpLine, QmpReply, ReplyError,
};
pub use share::{MountsTable, Share, ShareList, ShareRole, share_list};

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod fixtures;
