//! Checks shared by the path types: what makes a `/`-separated component unusable.

/// What is wrong with a component of a normalised path.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flaw {
    /// Doubled or trailing `/`.
    Empty,
    /// `.`
    Current,
    /// `..`
    Parent,
}

/// The first empty, `.` or `..` component of `components` (separated by `/`) and its index.
pub fn first_flaw(components: &str) -> Option<(Flaw, usize)> {
    components
        .split('/')
        .enumerate()
        .find_map(|(index, part)| match part {
            "" => Some((Flaw::Empty, index)),
            "." => Some((Flaw::Current, index)),
            ".." => Some((Flaw::Parent, index)),
            _ => None,
        })
}

/// The first C0 control or DEL and its char index.
pub fn first_control(text: &str) -> Option<(usize, char)> {
    text.chars()
        .enumerate()
        .find(|&(_, ch)| ch.is_ascii_control())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testing;

    #[test]
    fn first_flaw_names_the_kind_and_index_of_the_first_bad_component() {
        testing::table(
            &[
                ("", Some((Flaw::Empty, 0))),
                ("a", None),
                ("a/b/c", None),
                ("..x/.y/...", None),
                ("/", Some((Flaw::Empty, 0))),
                ("a/", Some((Flaw::Empty, 1))),
                ("a//b", Some((Flaw::Empty, 1))),
                (".", Some((Flaw::Current, 0))),
                ("a/./b", Some((Flaw::Current, 1))),
                ("..", Some((Flaw::Parent, 0))),
                ("a/b/..", Some((Flaw::Parent, 2))),
                ("./..", Some((Flaw::Current, 0))),
            ],
            |text| first_flaw(text),
        );
    }

    #[test]
    fn first_control_counts_chars_and_includes_del_but_not_space_or_c1() {
        testing::table(
            &[
                ("", None),
                ("a b~é", None),
                ("\u{80}\u{9b}", None),
                ("a\0", Some((1, '\0'))),
                ("é\t", Some((1, '\t'))),
                ("\u{1f}", Some((0, '\u{1f}'))),
                ("ab\u{7f}", Some((2, '\u{7f}'))),
                ("a\nb\0", Some((1, '\n'))),
            ],
            |text| first_control(text),
        );
    }
}
