//! `TermSize`: the host terminal's window size; rows and columns are both non-zero. The proof is
//! in the field types, so the fields are public and [`TermSize::parse`] is the boundary for the
//! raw `u16` pair the kernel reports.

use std::num::NonZeroU16;

use crate::diagnostic::Diagnostic;

/// A terminal size with at least one row and one column.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TermSize {
    pub rows: NonZeroU16,
    pub cols: NonZeroU16,
}

/// Which dimension of a reported window size is zero. A tty without a size reports `0 x 0`.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum TermSizeError {
    #[error("bad terminal size: it has no rows")]
    NoRows,
    #[error("bad terminal size: it has no columns")]
    NoCols,
    #[error("bad terminal size: it has no rows and no columns")]
    Empty,
}

impl Diagnostic for TermSizeError {
    fn code(&self) -> &'static str {
        match self {
            Self::NoRows => "term-size.no-rows",
            Self::NoCols => "term-size.no-cols",
            Self::Empty => "term-size.empty",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::NoRows | Self::NoCols | Self::Empty => None,
        }
    }
}

impl TermSize {
    /// # Errors
    /// [`TermSizeError`] naming the zero dimension(s).
    pub fn parse(rows: u16, cols: u16) -> Result<Self, TermSizeError> {
        match (NonZeroU16::new(rows), NonZeroU16::new(cols)) {
            (Some(rows), Some(cols)) => Ok(Self { rows, cols }),
            (None, Some(_)) => Err(TermSizeError::NoRows),
            (Some(_), None) => Err(TermSizeError::NoCols),
            (None, None) => Err(TermSizeError::Empty),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::testing::{self, Entry};

    fn witness(e: &TermSizeError) -> &'static str {
        match e {
            TermSizeError::NoRows => "NoRows",
            TermSizeError::NoCols => "NoCols",
            TermSizeError::Empty => "Empty",
        }
    }

    fn errors() -> Vec<TermSizeError> {
        vec![
            TermSizeError::NoRows,
            TermSizeError::NoCols,
            TermSizeError::Empty,
        ]
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        errors().iter().map(Entry::of).collect()
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(&errors(), witness, &["NoRows", "NoCols", "Empty"]);
    }

    fn size(rows: u16, cols: u16) -> TermSize {
        TermSize {
            rows: NonZeroU16::new(rows).unwrap(),
            cols: NonZeroU16::new(cols).unwrap(),
        }
    }

    #[test]
    fn term_size_parse_needs_a_row_and_a_column() {
        testing::table(
            &[
                ((0, 0), Err(TermSizeError::Empty)),
                ((0, 1), Err(TermSizeError::NoRows)),
                ((0, u16::MAX), Err(TermSizeError::NoRows)),
                ((1, 0), Err(TermSizeError::NoCols)),
                ((u16::MAX, 0), Err(TermSizeError::NoCols)),
                ((1, 1), Ok(size(1, 1))),
                ((24, 80), Ok(size(24, 80))),
                ((u16::MAX, u16::MAX), Ok(size(u16::MAX, u16::MAX))),
            ],
            |(rows, cols)| TermSize::parse(*rows, *cols),
        );
    }
}
