//! `GuestPath`: an absolute path inside the guest, free of control characters.

use crate::diagnostic::Diagnostic;
use crate::domain::components::first_control;
use crate::refined::{CheckedStr, Refined, Refinement};

/// Marker of [`GuestPath`].
pub struct GuestPathShape;

/// A path in the guest's filesystem (e.g. the state share's mount point from the contract).
/// Not normalised: the guest resolves it, the host never does.
pub type GuestPath = Refined<String, GuestPathShape>;

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum GuestPathError {
    #[error("bad guest path: it isn't absolute")]
    NotAbsolute,
    /// `at` is a char index.
    #[error("bad guest path: control character at position {at}")]
    ControlCharacter { at: usize },
}

impl Diagnostic for GuestPathError {
    fn code(&self) -> &'static str {
        match self {
            Self::NotAbsolute => "guest-path.not-absolute",
            Self::ControlCharacter { .. } => "guest-path.control-character",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some("use an absolute path without control characters, e.g. /mnt/amoeboid".to_owned())
    }
}

impl GuestPathShape {
    /// Compile-time twin of [`Refinement::check`], for `refined!`.
    #[must_use]
    pub const fn check_const(value: &'static str) -> Option<CheckedStr<Self>> {
        let mut rest = value.as_bytes();
        let [b'/', tail @ ..] = rest else {
            return None;
        };
        rest = tail;
        // Bytes of multi-byte characters are all >= 0x80, so they are never controls.
        while let [byte, tail @ ..] = rest {
            if *byte < 0x20 || *byte == 0x7f {
                return None;
            }
            rest = tail;
        }
        Some(CheckedStr::new(value))
    }
}

impl Refinement<String> for GuestPathShape {
    type Error = GuestPathError;

    fn check(value: &String) -> Result<(), GuestPathError> {
        if !value.starts_with('/') {
            return Err(GuestPathError::NotAbsolute);
        }
        match first_control(value) {
            Some((at, _)) => Err(GuestPathError::ControlCharacter { at }),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::testing::{self, Entry};

    fn witness(e: &GuestPathError) -> &'static str {
        match e {
            GuestPathError::NotAbsolute => "NotAbsolute",
            GuestPathError::ControlCharacter { .. } => "ControlCharacter",
        }
    }

    fn errors() -> Vec<GuestPathError> {
        vec![
            GuestPathError::NotAbsolute,
            GuestPathError::ControlCharacter { at: 4 },
        ]
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        errors().iter().map(Entry::of).collect()
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(&errors(), witness, &["NotAbsolute", "ControlCharacter"]);
    }

    #[test]
    fn guest_paths_are_absolute_without_control_characters() {
        testing::table(
            &[
                ("", Err(GuestPathError::NotAbsolute)),
                ("a", Err(GuestPathError::NotAbsolute)),
                ("./mnt", Err(GuestPathError::NotAbsolute)),
                ("\0/a", Err(GuestPathError::NotAbsolute)),
                ("/", Ok(())),
                ("/mnt/amoeboid", Ok(())),
                ("/a b/日本", Ok(())),
                ("/a/../b//", Ok(())),
                ("/\u{80}", Ok(())),
                ("/\0", Err(GuestPathError::ControlCharacter { at: 1 })),
                ("/a\nb", Err(GuestPathError::ControlCharacter { at: 2 })),
                ("/é\t", Err(GuestPathError::ControlCharacter { at: 2 })),
                ("/a\u{1f}", Err(GuestPathError::ControlCharacter { at: 2 })),
                ("/a\u{7f}", Err(GuestPathError::ControlCharacter { at: 2 })),
            ],
            |text| GuestPath::parse((*text).to_owned()).map(|_| ()),
        );
    }

    #[test]
    fn check_const_agrees_with_check_at_every_edge() {
        for text in [
            "",
            "a",
            "/",
            "/mnt/amoeboid",
            "/a b/日本",
            "/\u{80}",
            "/\0",
            "/a\nb",
            "/é\t",
            "/a\u{1f}",
            "/a\u{7f}",
            "\0/a",
            " /a",
        ] {
            assert_eq!(
                GuestPathShape::check_const(text).is_some(),
                GuestPathShape::check(&text.to_owned()).is_ok(),
                "text {text:?}"
            );
        }
    }

    #[test]
    fn refined_constants_of_guest_paths_are_the_parsed_values() {
        assert_eq!(
            GuestPath::parse("/mnt/amoeboid".to_owned()),
            Ok(crate::refined!(GuestPath, "/mnt/amoeboid"))
        );
    }

    #[test]
    fn accepted_guest_paths_parse_back_to_themselves() {
        let path = GuestPath::parse("/mnt/amoeboid".to_owned()).unwrap();
        assert_eq!(GuestPath::parse(path.get().clone()), Ok(path));
    }
}
