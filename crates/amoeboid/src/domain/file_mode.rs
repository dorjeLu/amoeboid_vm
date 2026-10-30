//! `FileMode`: permission bits of a file the host writes, `0..=0o777` (no setuid, setgid or sticky).

use crate::diagnostic::Diagnostic;
use crate::refined::{Checked, Refined, Refinement};

/// All permission bits for user, group and others.
pub const MAX: u16 = 0o777;

/// Marker of the permission bit range.
pub struct ModeBits;

/// Permission bits proven to be in [`ModeBits`].
pub type FileMode = Refined<u16, ModeBits>;

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum FileModeError {
    #[error("bad file mode: {value:o} sets bits beyond 777")]
    TooLarge { value: u16 },
}

impl Diagnostic for FileModeError {
    fn code(&self) -> &'static str {
        match self {
            Self::TooLarge { .. } => "file-mode.too-large",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::TooLarge { .. } => {
                Some("use permission bits only, from 0 to 777 (octal)".to_owned())
            }
        }
    }
}

impl ModeBits {
    /// Compile-time twin of [`Refinement::check`], for `refined!`.
    #[must_use]
    pub const fn check_const(value: u16) -> Option<Checked<u16, Self>> {
        if value <= MAX {
            Some(Checked::new(value))
        } else {
            None
        }
    }
}

impl ModeBits {
    /// The permission bits of an OS mode word (`st_mode`): the file type, setuid, setgid and
    /// sticky bits are dropped, so the result always satisfies the predicate.
    #[must_use]
    pub fn of_st_mode(mode: u32) -> FileMode {
        let [low, high, ..] = (mode & u32::from(MAX)).to_le_bytes();
        Checked::new(u16::from_le_bytes([low, high])).refine()
    }
}

impl Refinement<u16> for ModeBits {
    type Error = FileModeError;

    fn check(value: &u16) -> Result<(), FileModeError> {
        match *value {
            0..=MAX => Ok(()),
            value @ 0o1000.. => Err(FileModeError::TooLarge { value }),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::refined;
    use crate::testing::{self, Entry};

    fn witness(e: &FileModeError) -> &'static str {
        match e {
            FileModeError::TooLarge { .. } => "TooLarge",
        }
    }

    fn errors() -> Vec<FileModeError> {
        vec![FileModeError::TooLarge { value: 0o1000 }]
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        errors().iter().map(Entry::of).collect()
    }

    #[test]
    fn an_os_mode_word_keeps_only_the_permission_bits() {
        testing::table(
            &[
                (0o100_644, 0o644),
                (0o040_755, 0o755),
                (0o104_755, 0o755),
                (0o101_777, 0o777),
                (0o100_000, 0),
                (0, 0),
                (0o777, 0o777),
                (u32::MAX, 0o777),
            ],
            |&mode| *ModeBits::of_st_mode(mode).get(),
        );
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(&errors(), witness, &["TooLarge"]);
    }

    #[test]
    fn file_mode_parse_accepts_permission_bits_and_refuses_special_bits() {
        testing::table(
            &[
                (0, Ok(0)),
                (1, Ok(1)),
                (0o600, Ok(0o600)),
                (0o776, Ok(0o776)),
                (0o777, Ok(0o777)),
                (0o1000, Err(FileModeError::TooLarge { value: 0o1000 })),
                (0o4755, Err(FileModeError::TooLarge { value: 0o4755 })),
                (u16::MAX, Err(FileModeError::TooLarge { value: u16::MAX })),
            ],
            |value| FileMode::parse(*value).map(FileMode::into_inner),
        );
    }

    #[test]
    fn accepted_file_modes_parse_back_to_themselves() {
        for raw in [0, 0o644, 0o777] {
            let mode = FileMode::parse(raw).unwrap();
            assert_eq!(FileMode::parse(*mode.get()), Ok(mode));
        }
    }

    #[test]
    fn check_const_agrees_with_check_at_every_edge() {
        for value in [0, 1, 0o776, 0o777, 0o1000, u16::MAX] {
            assert_eq!(
                ModeBits::check_const(value).is_some(),
                ModeBits::check(&value).is_ok(),
                "value {value:o}"
            );
        }
    }

    #[test]
    fn refined_constants_of_file_mode_are_the_parsed_values() {
        const PRIVATE: FileMode = refined!(FileMode, 0o600);
        assert_eq!(FileMode::parse(0o600), Ok(PRIVATE));
    }
}
