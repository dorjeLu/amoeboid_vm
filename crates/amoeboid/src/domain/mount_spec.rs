//! `MountSpec`: `PATH[:rw|:ro]` as the operator writes it for `start` or at the prompt.

use crate::diagnostic::Diagnostic;
use crate::domain::mode::Mode;
use crate::domain::spec_path::{SpecPath, SpecPathError};

/// What to share and how; the path is not resolved or observed yet (`Mount` is that, in D4).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MountSpec {
    pub path: SpecPath,
    pub mode: Mode,
}

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum MountSpecError {
    #[error("empty path in mount {spec:?}")]
    EmptyPath { spec: String },
    /// `at` is a byte offset into the path part.
    #[error("NUL byte in mount path at offset {at}")]
    NulByte { at: usize },
}

impl Diagnostic for MountSpecError {
    fn code(&self) -> &'static str {
        match self {
            Self::EmptyPath { .. } => "mount-spec.empty-path",
            Self::NulByte { .. } => "mount-spec.nul-byte",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some("write PATH[:rw|:ro], e.g. ~/dev/app or ~/dev/libs:ro (default policy: rw)".to_owned())
    }
}

impl MountSpec {
    /// A trailing `:rw` or `:ro` is the mode; any other text after the last `:` belongs to the
    /// path (`/odd:name` is a directory called `odd:name`).
    ///
    /// # Errors
    ///
    /// The path part is empty or contains a NUL byte.
    pub fn parse(spec: &str) -> Result<Self, MountSpecError> {
        let (raw, mode) = spec
            .rsplit_once(':')
            .and_then(|(head, tail)| Mode::from_suffix(tail).map(|mode| (head, mode)))
            .unwrap_or((spec, Mode::Rw));
        match SpecPath::parse(raw.to_owned()) {
            Ok(path) => Ok(Self { path, mode }),
            Err(SpecPathError::Empty) => Err(MountSpecError::EmptyPath {
                spec: spec.to_owned(),
            }),
            Err(SpecPathError::NulByte { at }) => Err(MountSpecError::NulByte { at }),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::testing::{self, Entry};

    fn witness(e: &MountSpecError) -> &'static str {
        match e {
            MountSpecError::EmptyPath { .. } => "EmptyPath",
            MountSpecError::NulByte { .. } => "NulByte",
        }
    }

    fn errors() -> Vec<MountSpecError> {
        vec![
            MountSpecError::EmptyPath {
                spec: ":ro".to_owned(),
            },
            MountSpecError::NulByte { at: 1 },
        ]
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        errors().iter().map(Entry::of).collect()
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(&errors(), witness, &["EmptyPath", "NulByte"]);
    }

    #[allow(clippy::unnecessary_wraps, reason = "a table cell is a Result")]
    fn spec(path: &str, mode: Mode) -> Result<MountSpec, MountSpecError> {
        Ok(MountSpec {
            path: SpecPath::parse(path.to_owned()).unwrap(),
            mode,
        })
    }

    fn empty(text: &str) -> Result<MountSpec, MountSpecError> {
        Err(MountSpecError::EmptyPath {
            spec: text.to_owned(),
        })
    }

    #[test]
    fn mount_specs_take_a_mode_suffix_only_when_it_is_exactly_rw_or_ro() {
        testing::table(
            &[
                ("/srv/data", spec("/srv/data", Mode::Rw)),
                ("/srv/data:ro", spec("/srv/data", Mode::Ro)),
                ("/srv/data:rw", spec("/srv/data", Mode::Rw)),
                ("rel/dir:ro", spec("rel/dir", Mode::Ro)),
                ("~/dev/app", spec("~/dev/app", Mode::Rw)),
                ("/odd:name", spec("/odd:name", Mode::Rw)),
                ("/odd:name:ro", spec("/odd:name", Mode::Ro)),
                ("/odd:ro:rw", spec("/odd:ro", Mode::Rw)),
                ("/srv/../srv/x/", spec("/srv/../srv/x/", Mode::Rw)),
                ("/srv:RO", spec("/srv:RO", Mode::Rw)),
                ("/srv:", spec("/srv:", Mode::Rw)),
                ("/srv: ro", spec("/srv: ro", Mode::Rw)),
                (":", spec(":", Mode::Rw)),
                ("::ro", spec(":", Mode::Ro)),
                (":ro", empty(":ro")),
                (":rw", empty(":rw")),
                ("", empty("")),
                ("a\0b:ro", Err(MountSpecError::NulByte { at: 1 })),
                ("\0", Err(MountSpecError::NulByte { at: 0 })),
            ],
            |text| MountSpec::parse(text),
        );
    }
}
