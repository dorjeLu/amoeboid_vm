//! `RunDirName`: `amb_<VmName>`, the VM's directory under the runtime dir. Derived, never parsed.

use std::fmt;

use crate::domain::abs_path::{Segment, sealed};
use crate::domain::vm_name::VmName;
use crate::domain::word::{self, Fragment, Word};
use crate::refined;

/// Prefix of every run directory name; a [`Word`], so the name stays one component.
pub const PREFIX: Word = refined!(Word, "amb_");

/// The run directory name of a VM: a file name of at most 44 bytes, no `/`.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct RunDirName(String);

impl RunDirName {
    /// Total: every [`VmName`] yields a valid directory name.
    #[must_use]
    pub fn of(vm: &VmName) -> Self {
        Self(word::concat(&[Fragment::Text(PREFIX), Fragment::Vm(vm)]))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for RunDirName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RunDirName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// `amb_` plus a VM name is one component.
impl sealed::Sealed for RunDirName {}

impl Segment for RunDirName {
    fn segment(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::domain::abs_path::tests::is_segment;
    use crate::domain::vm_name::MAX_LEN;
    use crate::testing;

    fn vm(text: &str) -> VmName {
        VmName::parse(text.to_owned()).unwrap()
    }

    #[test]
    fn run_dir_name_is_amb_prefix_and_vm_name() {
        let longest = "z".repeat(MAX_LEN);
        let expected_longest = format!("amb_{longest}");
        testing::table(
            &[
                ("a", "amb_a".to_owned()),
                ("proj-1", "amb_proj-1".to_owned()),
                ("A9", "amb_A9".to_owned()),
                (longest.as_str(), expected_longest),
            ],
            |name| RunDirName::of(&vm(name)).as_str().to_owned(),
        );
    }

    #[test]
    fn run_dir_name_of_a_forty_char_vm_is_forty_four_bytes() {
        let longest = RunDirName::of(&vm(&"z".repeat(MAX_LEN)));
        assert_eq!(longest.as_str().len(), 44);
        assert_eq!(PREFIX.get().len(), 4);
    }

    #[test]
    fn run_dir_name_is_a_single_path_component() {
        for raw in ["a", "a-b", "Z9"] {
            let dir = RunDirName::of(&vm(raw));
            assert!(is_segment(dir.segment()), "{dir}");
            assert_eq!(dir.as_ref(), dir.as_str());
            assert_eq!(dir.to_string(), dir.as_str());
        }
    }
}
