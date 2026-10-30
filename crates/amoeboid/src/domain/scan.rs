//! The shape shared by the name grammars: a start class, then a class for every other char.

/// Why a text is not `START REST*`; the callers add their own limits and map to their own enum.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Misfit {
    Empty,
    BadStart {
        ch: char,
    },
    /// `at` counts chars from the beginning of the text.
    BadChar {
        ch: char,
        at: usize,
    },
}

/// Checks `text` against `START REST*` and reports the first offender.
pub fn scan(text: &str, start: fn(char) -> bool, rest: fn(char) -> bool) -> Result<(), Misfit> {
    let Some(first) = text.chars().next() else {
        return Err(Misfit::Empty);
    };
    if !start(first) {
        return Err(Misfit::BadStart { ch: first });
    }
    match text.chars().enumerate().skip(1).find(|&(_, ch)| !rest(ch)) {
        Some((at, ch)) => Err(Misfit::BadChar { ch, at }),
        None => Ok(()),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testing;

    #[test]
    fn scan_reports_the_first_offender_by_char_position() {
        let start: fn(char) -> bool = |c| c.is_ascii_lowercase();
        let rest: fn(char) -> bool = |c| c.is_ascii_lowercase() || c == '-';
        testing::table(
            &[
                ("", Err(Misfit::Empty)),
                ("a", Ok(())),
                ("a-b", Ok(())),
                ("-a", Err(Misfit::BadStart { ch: '-' })),
                ("é", Err(Misfit::BadStart { ch: 'é' })),
                ("aB", Err(Misfit::BadChar { ch: 'B', at: 1 })),
                ("éé", Err(Misfit::BadStart { ch: 'é' })),
                ("aéB", Err(Misfit::BadChar { ch: 'é', at: 1 })),
                ("a-é", Err(Misfit::BadChar { ch: 'é', at: 2 })),
            ],
            |text| scan(text, start, rest),
        );
    }
}
