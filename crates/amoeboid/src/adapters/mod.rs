//! Real implementations of the ports (design §Ports and adapters): the only place where refined
//! values meet raw paths and the operating system.

#![allow(clippy::disallowed_types, reason = "boundary: refines raw input")]

mod fs;
mod guest;
mod lock;
mod nix;
mod procs;
mod qmp;
mod session;
mod ssh;
mod system;

pub use fs::StdFs;
pub use guest::ShareDelivery;
pub use lock::{HeldFile, StdLock};
pub use nix::StdNix;
pub use procs::StdProcs;
pub use qmp::StdQmp;
pub use session::StdSession;
pub use ssh::StdSsh;
pub use system::StdSystem;
