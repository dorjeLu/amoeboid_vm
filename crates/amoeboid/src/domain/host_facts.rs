//! What the system answered when asked about the host (design §Resiliency/Preflight): plain
//! values from the `System` port, decided on by pure code. Nothing here looks at the system.

use std::fmt;

use crate::domain::FileName;
use crate::refined;

/// A program the library runs by name, found through `PATH`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Tool {
    Nix,
    SshKeygen,
}

impl Tool {
    pub const ALL: [Self; 2] = [Self::Nix, Self::SshKeygen];

    /// The file name looked up in each `PATH` directory.
    #[must_use]
    pub const fn file_name(self) -> FileName {
        match self {
            Self::Nix => refined!(FileName, "nix"),
            Self::SshKeygen => refined!(FileName, "ssh-keygen"),
        }
    }
}

impl fmt::Display for Tool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.file_name().get())
    }
}

/// Whether an executable of that name is in a `PATH` directory.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ToolPresence {
    Found,
    Missing,
}

/// What opening `/dev/kvm` for reading and writing would meet.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum KvmAccess {
    Usable,
    /// No such device: the module isn't loaded, or the host has no virtualisation.
    Absent,
    /// The device exists but this user may not use it.
    Denied,
}

/// A kernel switch as `/proc/sys` shows it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Switch {
    /// The file isn't there: this kernel has no such switch.
    Absent,
    /// `0`.
    Off,
    /// Anything else.
    On,
}

/// The switches that decide whether an unprivileged process may create a user namespace
/// (virtiofsd `--sandbox=namespace` needs one).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct UsernsSwitches {
    /// `user/max_user_namespaces`: `Off` means none may exist.
    pub max_namespaces: Switch,
    /// `kernel/unprivileged_userns_clone` (Debian and derivatives): `Off` forbids it.
    pub unprivileged_clone: Switch,
    /// `kernel/apparmor_restrict_unprivileged_userns` (Ubuntu 23.10+): `On` forbids it for
    /// programs without an `AppArmor` profile.
    pub apparmor_restricts: Switch,
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testing;

    #[test]
    fn tools_are_looked_up_under_their_own_names() {
        testing::table(
            &[(Tool::Nix, "nix"), (Tool::SshKeygen, "ssh-keygen")],
            |tool| {
                assert_eq!(tool.file_name().get(), &tool.to_string());
                tool.file_name().get().to_owned()
            },
        );
        assert_eq!(Tool::ALL, [Tool::Nix, Tool::SshKeygen]);
    }
}
