//! Effect ports (design §Ports and adapters): traits the use cases are written against, so
//! tests can swap the real implementation (in `adapters`) for a fake. Pure modules never import
//! this module.

pub mod guest_delivery;
pub mod host_fs;
pub mod nix;
pub mod procs;
pub mod qmp;
pub mod session;
pub mod ssh;
pub mod system;
pub mod vm_lock;

use std::fmt;
use std::io;

/// Why a system call failed, comparable: `io::Error` itself is not `Eq`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cause {
    pub kind: io::ErrorKind,
    /// The raw OS error number, when the error came from the OS.
    pub errno: Option<i32>,
}

impl Cause {
    #[must_use]
    pub fn of(error: &io::Error) -> Self {
        Self {
            kind: error.kind(),
            errno: error.raw_os_error(),
        }
    }
}

/// `permission denied`, `no storage space`, ... (the kind's own words).
impl fmt::Display for Cause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.kind.fmt(f)
    }
}
