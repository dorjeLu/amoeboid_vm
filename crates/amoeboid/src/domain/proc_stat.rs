//! `parse_stat`: the start time out of a `/proc/PID/stat` line (`man 5 proc`).
//!
//! The line is `pid (comm) state ppid ...`. `comm` is the process's name and may hold anything
//! (spaces, parentheses, newlines), so the fixed fields are counted from the *last* `)`.

use crate::diagnostic::Diagnostic;
use crate::domain::{Pid, StartTime};

/// Fields after `(comm)` before `starttime` (field 22): the state is field 3, so `starttime` is
/// the 20th token after the `)`.
const START_TIME_INDEX: usize = 19;

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum StatError {
    /// No `(comm)` part: there is no `(` before the last `)`.
    #[error("bad process status line: it has no (name) part")]
    NoName,
    /// The text before `(` is not a pid.
    #[error("bad process status line: it doesn't start with a pid")]
    BadPid,
    #[error("bad process status line: it has no start time field")]
    NoStartTime,
    #[error("bad process status line: the start time isn't a number")]
    BadStartTime,
}

impl Diagnostic for StatError {
    fn code(&self) -> &'static str {
        match self {
            Self::NoName => "proc-stat.no-name",
            Self::BadPid => "proc-stat.bad-pid",
            Self::NoStartTime => "proc-stat.no-start-time",
            Self::BadStartTime => "proc-stat.bad-start-time",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some("check that /proc is the kernel's procfs".to_owned())
    }
}

/// The start time of the process `line` describes. The leading pid must be a valid [`Pid`]
/// (callers know which pid they asked for, so it is not returned).
///
/// # Errors
/// A line without `(name)`, a bad pid, too few fields, or a start time that is not an integer.
pub fn parse_stat(line: &str) -> Result<StartTime, StatError> {
    let (head, tail) = line.rsplit_once(')').ok_or(StatError::NoName)?;
    let (pid, _name) = head.split_once('(').ok_or(StatError::NoName)?;
    pid.trim()
        .parse::<u32>()
        .ok()
        .and_then(|raw| Pid::parse(raw).ok())
        .ok_or(StatError::BadPid)?;
    let field = tail
        .split_ascii_whitespace()
        .nth(START_TIME_INDEX)
        .ok_or(StatError::NoStartTime)?;
    let ticks = field.parse::<u64>().map_err(|_| StatError::BadStartTime)?;
    Ok(match StartTime::parse(ticks) {
        Ok(start) => start,
        Err(never) => match never {},
    })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::testing::{self, Entry};

    fn witness(e: &StatError) -> &'static str {
        match e {
            StatError::NoName => "NoName",
            StatError::BadPid => "BadPid",
            StatError::NoStartTime => "NoStartTime",
            StatError::BadStartTime => "BadStartTime",
        }
    }

    fn errors() -> Vec<StatError> {
        vec![
            StatError::NoName,
            StatError::BadPid,
            StatError::NoStartTime,
            StatError::BadStartTime,
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
            &["NoName", "BadPid", "NoStartTime", "BadStartTime"],
        );
    }

    /// The 19 tokens between the state and the start time are numbers; 29 follow it.
    fn line(pid: &str, name: &str, start: &str) -> String {
        let before: Vec<String> = (4..=21).map(|n| n.to_string()).collect();
        let after: Vec<String> = (23..=52).map(|n| n.to_string()).collect();
        format!(
            "{pid} ({name}) S {} {start} {}\n",
            before.join(" "),
            after.join(" ")
        )
    }

    #[allow(clippy::unnecessary_wraps, reason = "a table cell is a Result")]
    fn start(ticks: u64) -> Result<StartTime, StatError> {
        Ok(StartTime::parse(ticks).unwrap())
    }

    #[test]
    fn the_start_time_is_field_22_whatever_the_name_holds() {
        testing::table(
            &[
                (line("1", "systemd", "1"), start(1)),
                (line("4242", "sleep", "123456789"), start(123_456_789)),
                (line("7", "a b", "5"), start(5)),
                (line("7", "a) b", "5"), start(5)),
                (line("7", "(", "5"), start(5)),
                (line("7", ") (", "5"), start(5)),
                (line("7", "x) S 1 2 3 (y", "5"), start(5)),
                (line("7", "two\nlines\n", "5"), start(5)),
                (line("7", "", "0"), start(0)),
                (
                    line("2147483647", "max", "18446744073709551615"),
                    start(u64::MAX),
                ),
                (line("  9 ", "padded", "6"), start(6)),
            ],
            |text| parse_stat(text),
        );
    }

    #[test]
    fn a_real_status_line_parses() {
        let own = std::fs::read_to_string("/proc/self/stat").unwrap();
        assert!(parse_stat(&own).is_ok(), "{own}");
    }

    #[test]
    fn malformed_lines_map_to_the_typed_error_of_the_part_that_is_wrong() {
        let few = format!("5 (x) S {}", ["1"; 18].join(" "));
        let just_enough = format!("5 (x) S {} 77", ["1"; 18].join(" "));
        testing::table(
            &[
                ("", Err(StatError::NoName)),
                ("5 x S 1 2 3", Err(StatError::NoName)),
                ("5 (x S 1 2 3", Err(StatError::NoName)),
                ("5 x) S 1 2 3", Err(StatError::NoName)),
                ("(x) S 1 2 3", Err(StatError::BadPid)),
                ("abc (x) S", Err(StatError::BadPid)),
                ("0 (x) S", Err(StatError::BadPid)),
                ("-5 (x) S", Err(StatError::BadPid)),
                ("2147483648 (x) S", Err(StatError::BadPid)),
                ("5 (x)", Err(StatError::NoStartTime)),
                ("5 (x) S 1 2", Err(StatError::NoStartTime)),
                (few.as_str(), Err(StatError::NoStartTime)),
                (just_enough.as_str(), start(77)),
            ],
            |text| parse_stat(text),
        );
        let bad = |start: &str| parse_stat(&line("5", "x", start));
        assert_eq!(bad("abc"), Err(StatError::BadStartTime));
        assert_eq!(bad("-1"), Err(StatError::BadStartTime));
        assert_eq!(bad("18446744073709551616"), Err(StatError::BadStartTime));
    }

    proptest! {
        #[test]
        fn the_start_time_is_recovered_for_any_process_name(
            name in ".{0,40}",
            pid in 1..=i32::MAX,
            ticks in any::<u64>(),
        ) {
            let text = line(&pid.to_string(), &name, &ticks.to_string());
            prop_assert_eq!(parse_stat(&text), Ok(StartTime::parse(ticks).unwrap()));
        }
    }
}
