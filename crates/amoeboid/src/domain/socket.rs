//! `SocketName`: a socket's file name, no `/`, NUL or whitespace, ending in `.sock`; and
//! [`Socket`], the sockets amoeboid itself places in a runtime dir.
//!
//! `SocketName` is not a `Refined`: it is made only by the total [`SocketName::of`], which
//! concatenates proven [`Fragment`]s; there is no constructor from free text. Names read from the
//! build's contract are parsed into the closed [`Socket`] set instead ([`Socket::parse`]).

use std::fmt;

use crate::diagnostic::Diagnostic;
use crate::domain::abs_path::{Segment, sealed};
use crate::domain::share_tag::ShareTag;
use crate::domain::word::{self, Fragment, Word};
use crate::refined;

/// Every socket name ends in this.
pub const SUFFIX: Word = refined!(Word, ".sock");

const CONTROL: Word = refined!(Word, "control");
const STORE: Word = refined!(Word, "ro-store");
const STATE: Word = refined!(Word, "state");

/// A socket file name.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SocketName(String);

impl SocketName {
    /// The fragments' texts followed by `.sock`: free of `/`, NUL and whitespace because every
    /// fragment is.
    #[must_use]
    pub fn of(parts: &[Fragment]) -> Self {
        let mut text = word::concat(parts);
        text.push_str(SUFFIX.get());
        Self(text)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Shows the text only, like the refined types.
impl fmt::Debug for SocketName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl fmt::Display for SocketName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Ends in `.sock` without `/` or NUL, so it is one component and neither `.` nor `..`.
impl sealed::Sealed for SocketName {}

impl Segment for SocketName {
    fn segment(&self) -> &str {
        &self.0
    }
}

/// The sockets of a VM's runtime dir. The names must match `base.nix` (and the contract).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, serde::Deserialize)]
#[serde(try_from = "String")]
pub enum Socket {
    /// QMP, `microvm.socket`.
    Control,
    /// virtiofsd serving `/nix/store` read-only.
    Store,
    /// virtiofsd serving the guest-writable state dir.
    State,
    /// virtiofsd serving a shared directory.
    Share(ShareTag),
}

/// A socket name that is none of the sockets amoeboid places in a runtime dir.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum SocketError {
    #[error("unknown socket name {name:?}")]
    Unknown { name: String },
}

impl Diagnostic for SocketError {
    fn code(&self) -> &'static str {
        match self {
            Self::Unknown { .. } => "socket.unknown",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some(
            "use control.sock, ro-store.sock, state.sock or dN.sock (N from 1, no leading zeros)"
                .to_owned(),
        )
    }
}

/// For `#[serde(try_from)]`: contract JSON decodes straight into a `Socket`.
impl TryFrom<String> for Socket {
    type Error = SocketError;

    fn try_from(name: String) -> Result<Self, SocketError> {
        Self::parse(&name)
    }
}

impl Socket {
    /// The socket whose [`Socket::name`] is exactly `name`.
    ///
    /// # Errors
    /// Any other text.
    pub fn parse(name: &str) -> Result<Self, SocketError> {
        let found = name.strip_suffix(SUFFIX.get()).and_then(|stem| {
            if stem == *CONTROL.get() {
                Some(Self::Control)
            } else if stem == *STORE.get() {
                Some(Self::Store)
            } else if stem == *STATE.get() {
                Some(Self::State)
            } else {
                ShareTag::parse(stem).map(Self::Share)
            }
        });
        found.ok_or_else(|| SocketError::Unknown {
            name: name.to_owned(),
        })
    }

    /// `control.sock`, `ro-store.sock`, `state.sock`, `dN.sock`; at most 13 bytes.
    #[must_use]
    pub fn name(self) -> SocketName {
        SocketName::of(&[match self {
            Self::Control => Fragment::Text(CONTROL),
            Self::Store => Fragment::Text(STORE),
            Self::State => Fragment::Text(STATE),
            Self::Share(tag) => Fragment::Tag(tag),
        }])
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use std::num::NonZeroU16;

    use super::*;
    use crate::domain::abs_path::tests::is_segment;
    use crate::testing::{self, Entry};

    fn witness(e: &SocketError) -> &'static str {
        match e {
            SocketError::Unknown { .. } => "Unknown",
        }
    }

    fn errors() -> Vec<SocketError> {
        vec![SocketError::Unknown {
            name: "x.sock".to_owned(),
        }]
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        errors().iter().map(Entry::of).collect()
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(&errors(), witness, &["Unknown"]);
    }

    #[test]
    fn socket_parse_accepts_exactly_the_names_sockets_have() {
        let unknown = |name: &str| {
            Err(SocketError::Unknown {
                name: name.to_owned(),
            })
        };
        let tag = |n| Socket::Share(ShareTag::nth(NonZeroU16::new(n).unwrap()));
        testing::table(
            &[
                ("control.sock", Ok(Socket::Control)),
                ("ro-store.sock", Ok(Socket::Store)),
                ("state.sock", Ok(Socket::State)),
                ("d1.sock", Ok(tag(1))),
                ("d65535.sock", Ok(tag(u16::MAX))),
                ("", unknown("")),
                (".sock", unknown(".sock")),
                ("control", unknown("control")),
                ("control.sock.", unknown("control.sock.")),
                ("Control.sock", unknown("Control.sock")),
                ("control.SOCK", unknown("control.SOCK")),
                (" control.sock", unknown(" control.sock")),
                ("d0.sock", unknown("d0.sock")),
                ("d01.sock", unknown("d01.sock")),
                ("d65536.sock", unknown("d65536.sock")),
                ("d.sock", unknown("d.sock")),
                ("../control.sock", unknown("../control.sock")),
                ("a/d1.sock", unknown("a/d1.sock")),
                ("d1\0.sock", unknown("d1\0.sock")),
                ("other.sock", unknown("other.sock")),
            ],
            |name| Socket::parse(name),
        );
    }

    #[test]
    fn socket_try_from_string_is_parse() {
        assert_eq!(Socket::try_from("state.sock".to_owned()), Ok(Socket::State));
        assert_eq!(
            Socket::try_from("x".to_owned()),
            Err(SocketError::Unknown {
                name: "x".to_owned()
            })
        );
    }

    pub(crate) fn every_socket() -> Vec<Socket> {
        let tag = |n| Socket::Share(ShareTag::nth(NonZeroU16::new(n).unwrap()));
        vec![
            Socket::Control,
            Socket::Store,
            Socket::State,
            tag(1),
            tag(9),
            tag(10),
            tag(u16::MAX),
        ]
    }

    #[test]
    fn sockets_are_named_as_in_base_nix() {
        testing::table(
            &[
                (Socket::Control, "control.sock".to_owned()),
                (Socket::Store, "ro-store.sock".to_owned()),
                (Socket::State, "state.sock".to_owned()),
                (
                    Socket::Share(ShareTag::nth(NonZeroU16::new(65_535).unwrap())),
                    "d65535.sock".to_owned(),
                ),
            ],
            |socket| {
                let name = socket.name();
                assert_eq!(format!("{name:?}"), format!("{:?}", name.as_str()));
                assert_eq!(name.to_string(), name.as_str());
                name.as_str().to_owned()
            },
        );
    }

    #[test]
    fn names_of_any_fragments_are_one_component_without_whitespace_and_end_in_the_suffix() {
        let word = |text| Fragment::Text(Word::parse(text).unwrap());
        let number = NonZeroU16::new(7).unwrap();
        testing::table(
            &[
                (vec![], ".sock".to_owned()),
                (vec![word("a-b")], "a-b.sock".to_owned()),
                (
                    vec![
                        word("x"),
                        Fragment::Number(number),
                        Fragment::Tag(ShareTag::nth(number)),
                        Fragment::Port(crate::domain::LoopbackPort::parse(65_535).unwrap()),
                    ],
                    "x7d765535.sock".to_owned(),
                ),
            ],
            |parts| {
                let name = SocketName::of(parts);
                assert!(is_segment(name.segment()), "{name}");
                assert!(!name.as_str().contains(char::is_whitespace), "{name}");
                name.as_str().to_owned()
            },
        );
    }

    #[test]
    fn every_socket_name_parses_back_and_is_one_path_component() {
        for socket in every_socket() {
            let name = socket.name();
            assert_eq!(Socket::parse(name.as_str()), Ok(socket));
            assert!(is_segment(name.segment()), "{name}");
        }
    }
}
