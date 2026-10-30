//! `Word`: a literal that is safe inside a generated space-separated or path-like text, and
//! [`Fragment`], the proven pieces from which `QemuArg`s, `SocketName`s and directory names are
//! composed.
//!
//! A `Word` is non-empty ASCII graphic text without `/` (so no whitespace, NUL or control
//! character either). It is written as a `refined!` constant, so the check runs at compile time.
//! Concatenating fragments is total: every fragment is non-empty and free of whitespace, `/` and
//! NUL, and so is any sequence of them.

use std::num::NonZeroU16;

use crate::diagnostic::Diagnostic;
use crate::domain::{GuestPort, LoopbackPort, ShareTag, Uid, VmName};
use crate::refined::{Checked, Refined, Refinement};

/// Marker of the word grammar.
pub struct WordShape;

/// A literal proven to be a word.
pub type Word = Refined<&'static str, WordShape>;

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum WordError {
    #[error("bad word: it is empty")]
    Empty,
    /// `at` is a byte offset.
    #[error("bad word: '/' at offset {at}")]
    Separator { at: usize },
    /// Whitespace, a control character or a non-ASCII character; `at` is a byte offset.
    #[error("bad word: byte outside printable ASCII at offset {at}")]
    NotGraphic { at: usize },
}

impl Diagnostic for WordError {
    fn code(&self) -> &'static str {
        match self {
            Self::Empty => "word.empty",
            Self::Separator { .. } => "word.separator",
            Self::NotGraphic { .. } => "word.not-graphic",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some("use printable ASCII without spaces or '/'".to_owned())
    }
}

impl WordShape {
    /// Compile-time twin of [`Refinement::check`], for `refined!`.
    #[must_use]
    pub const fn check_const(value: &'static str) -> Option<Checked<&'static str, Self>> {
        let mut rest = value.as_bytes();
        if rest.is_empty() {
            return None;
        }
        while let [byte, tail @ ..] = rest {
            if !matches!(*byte, b'!'..=b'~') || *byte == b'/' {
                return None;
            }
            rest = tail;
        }
        Some(Checked::new(value))
    }
}

impl Refinement<&'static str> for WordShape {
    type Error = WordError;

    fn check(value: &&'static str) -> Result<(), WordError> {
        if value.is_empty() {
            return Err(WordError::Empty);
        }
        match value
            .bytes()
            .enumerate()
            .find(|&(_, byte)| !matches!(byte, b'!'..=b'~') || byte == b'/')
        {
            Some((at, b'/')) => Err(WordError::Separator { at }),
            Some((at, _)) => Err(WordError::NotGraphic { at }),
            None => Ok(()),
        }
    }
}

/// A piece of generated text that is already a proven word, or renders as one: the digits of a
/// number, a share tag (`dN`) or a VM name (`[A-Za-z0-9-]`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fragment<'a> {
    Text(Word),
    Tag(ShareTag),
    Number(NonZeroU16),
    Port(LoopbackPort),
    Guest(GuestPort),
    Uid(Uid),
    Vm(&'a VmName),
}

impl Fragment<'_> {
    fn push_to(self, out: &mut String) {
        match self {
            Self::Text(word) => out.push_str(word.get()),
            Self::Uid(uid) => out.push_str(&uid.to_string()),
            Self::Vm(vm) => out.push_str(vm.get()),
            Self::Tag(tag) => out.push_str(&tag.to_string()),
            Self::Number(number) => out.push_str(&number.to_string()),
            Self::Port(port) => out.push_str(&port.to_string()),
            Self::Guest(port) => out.push_str(&port.to_string()),
        }
    }
}

/// The fragments' texts joined: non-empty if `parts` is, and free of whitespace, `/` and NUL.
#[must_use]
pub(crate) fn concat(parts: &[Fragment]) -> String {
    let mut out = String::new();
    for part in parts {
        part.push_to(&mut out);
    }
    out
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::refined;
    use crate::testing::{self, Entry};

    fn witness(e: &WordError) -> &'static str {
        match e {
            WordError::Empty => "Empty",
            WordError::Separator { .. } => "Separator",
            WordError::NotGraphic { .. } => "NotGraphic",
        }
    }

    fn errors() -> Vec<WordError> {
        vec![
            WordError::Empty,
            WordError::Separator { at: 1 },
            WordError::NotGraphic { at: 2 },
        ]
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        errors().iter().map(Entry::of).collect()
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(&errors(), witness, &["Empty", "Separator", "NotGraphic"]);
    }

    /// Texts on each side of every edge of the grammar.
    const TEXTS: [&str; 17] = [
        "", "!", "~", "-netdev", "a=b,c:d", ".sock", " ", "a b", "a\tb", "a\0b", "\x7f", "\x1f",
        "é", "a\u{a0}", "/", "a/b", "ab/",
    ];

    #[test]
    fn word_parse_accepts_printable_ascii_without_slash_and_names_the_violated_edge() {
        testing::table(
            &[
                ("", Err(WordError::Empty)),
                ("!", Ok(())),
                ("~", Ok(())),
                ("-netdev", Ok(())),
                ("a=b,c:d", Ok(())),
                (".sock", Ok(())),
                (" ", Err(WordError::NotGraphic { at: 0 })),
                ("a b", Err(WordError::NotGraphic { at: 1 })),
                ("a\tb", Err(WordError::NotGraphic { at: 1 })),
                ("a\0b", Err(WordError::NotGraphic { at: 1 })),
                ("\x7f", Err(WordError::NotGraphic { at: 0 })),
                ("\x1f", Err(WordError::NotGraphic { at: 0 })),
                ("aé", Err(WordError::NotGraphic { at: 1 })),
                ("a\u{a0}", Err(WordError::NotGraphic { at: 1 })),
                ("/", Err(WordError::Separator { at: 0 })),
                ("a/b", Err(WordError::Separator { at: 1 })),
                ("ab/ ", Err(WordError::Separator { at: 2 })),
                ("a b/", Err(WordError::NotGraphic { at: 1 })),
            ],
            |text| Word::parse(*text).map(|_| ()),
        );
    }

    #[test]
    fn check_const_agrees_with_check_at_every_edge() {
        for text in TEXTS {
            assert_eq!(
                WordShape::check_const(text).is_some(),
                WordShape::check(&text).is_ok(),
                "text {text:?}"
            );
        }
    }

    #[test]
    fn refined_constants_of_words_are_the_parsed_values() {
        const DEVICE: Word = refined!(Word, "-device");
        assert_eq!(Word::parse("-device"), Ok(DEVICE));
    }

    fn word(text: &'static str) -> Fragment<'static> {
        Fragment::Text(Word::parse(text).unwrap())
    }

    fn number(n: u16) -> NonZeroU16 {
        NonZeroU16::new(n).unwrap()
    }

    #[test]
    fn fragments_render_as_text_tag_digits_port_uid_and_vm() {
        let vm = VmName::parse("my-vm".to_owned()).unwrap();
        testing::table(
            &[
                (vec![], String::new()),
                (vec![word("-x")], "-x".to_owned()),
                (
                    vec![Fragment::Tag(ShareTag::nth(number(12)))],
                    "d12".to_owned(),
                ),
                (vec![Fragment::Number(number(65_535))], "65535".to_owned()),
                (
                    vec![Fragment::Guest(GuestPort::parse(22).unwrap())],
                    "22".to_owned(),
                ),
                (
                    vec![Fragment::Uid(Uid::parse(1000).unwrap())],
                    "1000".to_owned(),
                ),
                (vec![Fragment::Vm(&vm)], "my-vm".to_owned()),
                (
                    vec![Fragment::Port(LoopbackPort::parse(22).unwrap())],
                    "22".to_owned(),
                ),
                (
                    vec![
                        word("amo"),
                        Fragment::Number(number(3)),
                        word(",path="),
                        Fragment::Tag(ShareTag::nth(number(3))),
                    ],
                    "amo3,path=d3".to_owned(),
                ),
            ],
            |parts| concat(parts),
        );
    }

    #[test]
    fn concatenated_fragments_stay_words() {
        let vm = VmName::parse("Z9-".repeat(13)).unwrap();
        let parts = [
            word("a"),
            Fragment::Tag(ShareTag::nth(number(1))),
            Fragment::Number(number(u16::MAX)),
            Fragment::Port(LoopbackPort::parse(u16::MAX).unwrap()),
            Fragment::Uid(Uid::parse(u32::MAX - 1).unwrap()),
            Fragment::Vm(&vm),
        ];
        let text = concat(&parts);
        assert_ne!(text, "");
        assert!(text.bytes().all(|b| matches!(b, b'!'..=b'~') && b != b'/'));
    }
}
