//! `TreeFile`: one regular file read from a host-owned directory tree.

use crate::domain::guest_rel::GuestRel;

/// A regular file of a tree, with its path below the tree's root and its content.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TreeFile {
    pub rel: GuestRel,
    pub bytes: Vec<u8>,
}
