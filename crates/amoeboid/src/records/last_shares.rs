//! `LastShares`: the shares of the VM's last start, read back from `<data>/NAME/mounts`.
//!
//! The file is the text of `plan::MountsTable` (`dN<TAB>mode<TAB>path<LF>` per share), kept
//! outside the guest's reach. The paths are as recorded, not observed again: they go through
//! `Shares::validate` like any other request.

use std::num::NonZeroU16;

use crate::diagnostic::Diagnostic;
use crate::domain::{AbsDir, AbsPathError, Mode, MountRequest, ShareTag};

/// The recorded shares in mount order. Only [`LastShares::parse`] makes one; the tags were
/// `d1, d2, ...` in order, so there are at most 65535.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LastShares(Vec<MountRequest>);

/// What is wrong with a `mounts` file; `line` counts from 1.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum LastSharesError {
    #[error("line {line}: the line doesn't end with a newline")]
    Unterminated { line: usize },
    #[error("line {line}: expected 3 tab-separated fields, found {found}")]
    Fields { line: usize, found: usize },
    #[error("line {line}: expected the tag {expected}, found {found:?}")]
    Tag {
        line: usize,
        expected: ShareTag,
        found: String,
    },
    #[error("line {line}: the mode is {found:?}, not rw or ro")]
    Mode { line: usize, found: String },
    #[error("line {line}: {error}")]
    Path { line: usize, error: AbsPathError },
    #[error("line {line}: more than 65535 shares")]
    TooMany { line: usize },
}

impl Diagnostic for LastSharesError {
    fn code(&self) -> &'static str {
        match self {
            Self::Unterminated { .. } => "mounts-record.unterminated",
            Self::Fields { .. } => "mounts-record.fields",
            Self::Tag { .. } => "mounts-record.tag",
            Self::Mode { .. } => "mounts-record.mode",
            Self::Path { .. } => "mounts-record.path",
            Self::TooMany { .. } => "mounts-record.too-many",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some("fix the file, or delete it to forget the last shares".to_owned())
    }
}

impl LastShares {
    /// # Errors
    ///
    /// The first line that is not `dN<TAB>rw|ro<TAB>/absolute/path`, where `N` is its line number;
    /// a last line without its newline.
    pub fn parse(text: &str) -> Result<Self, LastSharesError> {
        let mut shares = Vec::new();
        for (index, chunk) in text.split_inclusive('\n').enumerate() {
            let line = index.saturating_add(1);
            let Some(body) = chunk.strip_suffix('\n') else {
                return Err(LastSharesError::Unterminated { line });
            };
            shares.push(share(line, body)?);
        }
        Ok(Self(shares))
    }

    #[must_use]
    pub fn as_slice(&self) -> &[MountRequest] {
        &self.0
    }

    pub fn iter(&self) -> impl Iterator<Item = &MountRequest> {
        self.0.iter()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl IntoIterator for LastShares {
    type Item = MountRequest;
    type IntoIter = std::vec::IntoIter<MountRequest>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

fn share(line: usize, body: &str) -> Result<MountRequest, LastSharesError> {
    let Some(expected) = u16::try_from(line)
        .ok()
        .and_then(NonZeroU16::new)
        .map(ShareTag::nth)
    else {
        return Err(LastSharesError::TooMany { line });
    };
    let fields: Vec<&str> = body.split('\t').collect();
    let [tag, mode, path] = fields.as_slice() else {
        return Err(LastSharesError::Fields {
            line,
            found: fields.len(),
        });
    };
    if ShareTag::parse(tag) != Some(expected) {
        return Err(LastSharesError::Tag {
            line,
            expected,
            found: (*tag).to_owned(),
        });
    }
    let mode = Mode::from_suffix(mode).ok_or_else(|| LastSharesError::Mode {
        line,
        found: (*mode).to_owned(),
    })?;
    let path =
        AbsDir::parse((*path).to_owned()).map_err(|error| LastSharesError::Path { line, error })?;
    Ok(MountRequest { path, mode })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::plan::fixtures::{abs, list_of, shares, shares_from_golden};
    use crate::testing::{self, Entry};

    fn witness(e: &LastSharesError) -> &'static str {
        match e {
            LastSharesError::Unterminated { .. } => "Unterminated",
            LastSharesError::Fields { .. } => "Fields",
            LastSharesError::Tag { .. } => "Tag",
            LastSharesError::Mode { .. } => "Mode",
            LastSharesError::Path { .. } => "Path",
            LastSharesError::TooMany { .. } => "TooMany",
        }
    }

    fn errors() -> Vec<LastSharesError> {
        vec![
            LastSharesError::Unterminated { line: 2 },
            LastSharesError::Fields { line: 1, found: 2 },
            LastSharesError::Tag {
                line: 2,
                expected: ShareTag::nth(NonZeroU16::MIN.saturating_add(1)),
                found: "d3".to_owned(),
            },
            LastSharesError::Mode {
                line: 1,
                found: "rwx".to_owned(),
            },
            LastSharesError::Path {
                line: 3,
                error: AbsPathError::NotAbsolute,
            },
            LastSharesError::TooMany { line: 65_536 },
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
            &["Unterminated", "Fields", "Tag", "Mode", "Path", "TooMany"],
        );
    }

    fn request(path: &str, mode: Mode) -> MountRequest {
        MountRequest {
            path: abs(path),
            mode,
        }
    }

    #[test]
    fn well_formed_tables_parse_to_their_requests_in_order() {
        testing::table(
            &[
                ("", Ok(vec![])),
                ("d1\trw\t/srv/a\n", Ok(vec![request("/srv/a", Mode::Rw)])),
                (
                    "d1\trw\t/srv/a\nd2\tro\t/srv/a/b c\n",
                    Ok(vec![
                        request("/srv/a", Mode::Rw),
                        request("/srv/a/b c", Mode::Ro),
                    ]),
                ),
                ("d1\tro\t/\n", Ok(vec![request("/", Mode::Ro)])),
                (
                    "d1\trw\t/srv/c\rd:e=f\n",
                    Ok(vec![request("/srv/c\rd:e=f", Mode::Rw)]),
                ),
            ],
            |text| LastShares::parse(text).map(|parsed| parsed.into_iter().collect()),
        );
    }

    #[test]
    fn every_unterminated_or_misshapen_line_is_a_typed_error_naming_its_line() {
        testing::table(
            &[
                ("d1", Err(LastSharesError::Unterminated { line: 1 })),
                (
                    "d1\trw\t/a\nd2\tro\t/b",
                    Err(LastSharesError::Unterminated { line: 2 }),
                ),
                ("\n", Err(LastSharesError::Fields { line: 1, found: 1 })),
                (
                    "d1\trw\t/a\n\n",
                    Err(LastSharesError::Fields { line: 2, found: 1 }),
                ),
                (
                    "d1\trw\n",
                    Err(LastSharesError::Fields { line: 1, found: 2 }),
                ),
                (
                    "d1\trw\t/a\tx\n",
                    Err(LastSharesError::Fields { line: 1, found: 4 }),
                ),
            ],
            |text| LastShares::parse(text).map(|parsed| parsed.into_iter().collect::<Vec<_>>()),
        );
    }

    #[test]
    fn every_corrupt_tag_mode_or_path_is_a_typed_error_naming_its_line() {
        let tag = |n: u16| ShareTag::nth(NonZeroU16::new(n).unwrap());
        testing::table(
            &[
                (
                    "d2\trw\t/a\n",
                    Err(LastSharesError::Tag {
                        line: 1,
                        expected: tag(1),
                        found: "d2".to_owned(),
                    }),
                ),
                (
                    "d1\trw\t/a\nd1\trw\t/b\n",
                    Err(LastSharesError::Tag {
                        line: 2,
                        expected: tag(2),
                        found: "d1".to_owned(),
                    }),
                ),
                (
                    "\trw\t/a\n",
                    Err(LastSharesError::Tag {
                        line: 1,
                        expected: tag(1),
                        found: String::new(),
                    }),
                ),
                (
                    "d01\trw\t/a\n",
                    Err(LastSharesError::Tag {
                        line: 1,
                        expected: tag(1),
                        found: "d01".to_owned(),
                    }),
                ),
                (
                    "d1\trwx\t/a\n",
                    Err(LastSharesError::Mode {
                        line: 1,
                        found: "rwx".to_owned(),
                    }),
                ),
                (
                    "d1\tRW\t/a\n",
                    Err(LastSharesError::Mode {
                        line: 1,
                        found: "RW".to_owned(),
                    }),
                ),
                (
                    "d1\t\t/a\n",
                    Err(LastSharesError::Mode {
                        line: 1,
                        found: String::new(),
                    }),
                ),
                (
                    "d1\trw\ta\n",
                    Err(LastSharesError::Path {
                        line: 1,
                        error: AbsPathError::NotAbsolute,
                    }),
                ),
                (
                    "d1\trw\t\n",
                    Err(LastSharesError::Path {
                        line: 1,
                        error: AbsPathError::Empty,
                    }),
                ),
                (
                    "d1\trw\t/a\nd2\trw\t/b/\n",
                    Err(LastSharesError::Path {
                        line: 2,
                        error: AbsPathError::EmptyComponent { index: 1 },
                    }),
                ),
                (
                    "d1\trw\t/a/../b\n",
                    Err(LastSharesError::Path {
                        line: 1,
                        error: AbsPathError::ParentDir { index: 1 },
                    }),
                ),
            ],
            |text| LastShares::parse(text).map(|parsed| parsed.into_iter().collect::<Vec<_>>()),
        );
    }

    #[test]
    fn the_first_bad_line_wins_and_earlier_lines_are_not_returned() {
        assert_eq!(
            LastShares::parse("d1\trw\t/a\nd2\trw\n d3\n"),
            Err(LastSharesError::Fields { line: 2, found: 2 })
        );
    }

    #[test]
    fn the_65536th_share_is_refused_because_no_tag_is_left() {
        let line = |n: u32| format!("d{n}\trw\t/s\n");
        let mut text: String = (1..=65_535).map(line).collect();
        let full = LastShares::parse(&text).unwrap();
        assert_eq!(full.len(), 65_535);
        assert!(!full.is_empty());
        text.push_str(&line(65_536));
        assert_eq!(
            LastShares::parse(&text),
            Err(LastSharesError::TooMany { line: 65_536 })
        );
    }

    #[test]
    fn a_table_written_for_the_last_start_reads_back_as_its_shares() {
        for (paths, text) in [
            (vec![], ""),
            (
                vec![("/srv/a b", Mode::Rw), ("/srv/c\rd", Mode::Ro)],
                "d1\trw\t/srv/a b\nd2\tro\t/srv/c\rd\n",
            ),
        ] {
            let mounts = shares(&paths);
            let table = list_of(&mounts).mounts_table();
            assert_eq!(table.as_str(), text);
            let read = LastShares::parse(table.as_str()).unwrap();
            let expected: Vec<MountRequest> = paths
                .iter()
                .map(|&(path, mode)| request(path, mode))
                .collect();
            assert_eq!(read.as_slice(), expected.as_slice());
            assert_eq!(read.iter().count(), paths.len());
        }
    }

    #[test]
    fn the_python_golden_tables_read_back_as_their_mounts() {
        for case in testing::golden_cases("mounts_table", "json", "tsv").unwrap() {
            let input: serde_json::Value = serde_json::from_str(&case.input).unwrap();
            let mounts = shares_from_golden(input.get("mounts").unwrap());
            let expected: Vec<MountRequest> = mounts
                .iter()
                .map(|mount| MountRequest {
                    path: mount.dir.path().clone(),
                    mode: mount.mode,
                })
                .collect();
            assert_eq!(
                LastShares::parse(&case.output).map(|read| read.into_iter().collect::<Vec<_>>()),
                Ok(expected),
                "{}",
                case.name
            );
        }
    }
}
