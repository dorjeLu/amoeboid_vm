//! `ProgramName` and `ProcIdentity`: which process a recorded pid names. A pid alone is reused by
//! the kernel; the start time tells processes apart, and the executable's name checks that the
//! process is the program we started (a recycled pid can't be a qemu started after our qemu died).

use crate::diagnostic::Diagnostic;
use crate::domain::{Pid, StartTime};
use crate::refined::{Refined, Refinement};

/// Marker of an executable's file name.
pub struct ProgramShape;

/// The file name of a process's executable (`/proc/PID/exe`'s target), e.g.
/// `qemu-system-x86_64`: not empty, no `/`, no NUL. The runner renames `argv[0]` of qemu but not
/// the executable, which is why the executable is what counts.
pub type ProgramName = Refined<String, ProgramShape>;

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum ProgramNameError {
    #[error("bad program name: it is empty")]
    Empty,
    /// `at` is a byte offset.
    #[error("bad program name: '/' at offset {at}")]
    Separator { at: usize },
    /// `at` is a byte offset.
    #[error("bad program name: NUL byte at offset {at}")]
    NulByte { at: usize },
}

impl Diagnostic for ProgramNameError {
    fn code(&self) -> &'static str {
        match self {
            Self::Empty => "program-name.empty",
            Self::Separator { .. } => "program-name.separator",
            Self::NulByte { .. } => "program-name.nul-byte",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some("give the file name of the executable, without its directory".to_owned())
    }
}

impl Refinement<String> for ProgramShape {
    type Error = ProgramNameError;

    fn check(value: &String) -> Result<(), ProgramNameError> {
        if value.is_empty() {
            return Err(ProgramNameError::Empty);
        }
        match value.find(['/', '\0']) {
            Some(at) if value.get(at..).is_some_and(|rest| rest.starts_with('/')) => {
                Err(ProgramNameError::Separator { at })
            }
            Some(at) => Err(ProgramNameError::NulByte { at }),
            None => Ok(()),
        }
    }
}

/// A process we started: its pid, when it started, and what it runs. Equal on all three or it is
/// not the same process. Recorded in the run record; signals go only to a process whose current
/// identity equals the recorded one.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ProcIdentity {
    pub pid: Pid,
    pub start: StartTime,
    pub program: ProgramName,
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::testing::{self, Entry};

    fn witness(e: &ProgramNameError) -> &'static str {
        match e {
            ProgramNameError::Empty => "Empty",
            ProgramNameError::Separator { .. } => "Separator",
            ProgramNameError::NulByte { .. } => "NulByte",
        }
    }

    fn errors() -> Vec<ProgramNameError> {
        vec![
            ProgramNameError::Empty,
            ProgramNameError::Separator { at: 3 },
            ProgramNameError::NulByte { at: 1 },
        ]
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        errors().iter().map(Entry::of).collect()
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(&errors(), witness, &["Empty", "Separator", "NulByte"]);
    }

    #[test]
    fn program_name_parse_accepts_a_file_name_and_names_the_violated_edge() {
        testing::table(
            &[
                ("", Err(ProgramNameError::Empty)),
                ("sleep", Ok(())),
                ("qemu-system-x86_64", Ok(())),
                ("microvm@host", Ok(())),
                ("a b", Ok(())),
                ("日本", Ok(())),
                ("sleep (deleted)", Ok(())),
                ("/", Err(ProgramNameError::Separator { at: 0 })),
                ("bin/sleep", Err(ProgramNameError::Separator { at: 3 })),
                ("sleep\0", Err(ProgramNameError::NulByte { at: 5 })),
                ("é\0/", Err(ProgramNameError::NulByte { at: 2 })),
                ("a/\0", Err(ProgramNameError::Separator { at: 1 })),
            ],
            |text| ProgramName::parse((*text).to_owned()).map(|_| ()),
        );
    }

    #[test]
    fn identities_are_equal_only_when_pid_start_and_program_all_are() {
        let make = |pid: u32, start: u64, program: &str| ProcIdentity {
            pid: Pid::parse(pid).unwrap(),
            start: StartTime::parse(start).unwrap(),
            program: ProgramName::parse(program.to_owned()).unwrap(),
        };
        let base = make(10, 5, "sleep");
        assert_eq!(base, make(10, 5, "sleep"));
        assert_ne!(base, make(11, 5, "sleep"));
        assert_ne!(base, make(10, 6, "sleep"));
        assert_ne!(base, make(10, 5, "qemu"));
    }
}
