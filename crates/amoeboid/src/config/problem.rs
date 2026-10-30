//! What can be wrong with a `run_config.kdl`: where (`Location`), what (`ProblemKind`), and the
//! error a whole decode ends with (`ConfigError`). Layers: [`ConfigError`] holds
//! [`Problem`]s, each a [`ProblemKind`] at a [`Location`], wrapping the domain errors.

use std::fmt;

use crate::diagnostic::Diagnostic;
use crate::domain::{
    BaseSpecError, FixedPortError, GidError, HomeRelError, ResolveError, SecondsError,
    SpecPathError, UidError, UserNameError,
};
use crate::kdl::{ParseError, Value};
use crate::rails::NonEmpty;

/// A key of `run_config.kdl`, at any level.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    User,
    Uid,
    Gid,
    BaseNixConfig,
    DefaultMounts,
    HomeFiles,
    Ssh,
    Port,
    Timeouts,
    Boot,
    Stop,
    Path,
    Mode,
    Linger,
    HomeStorage,
}

impl Key {
    /// The spelling in the file.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Uid => "uid",
            Self::Gid => "gid",
            Self::BaseNixConfig => "base-nix-config",
            Self::DefaultMounts => "default-mounts",
            Self::HomeFiles => "home-files",
            Self::Ssh => "ssh",
            Self::Port => "port",
            Self::Timeouts => "timeouts",
            Self::Boot => "boot",
            Self::Stop => "stop",
            Self::Path => "path",
            Self::Mode => "mode",
            Self::Linger => "linger",
            Self::HomeStorage => "home-storage",
        }
    }
}

/// One step into the document: a key, or the index of a list item.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Segment {
    Key(Key),
    Item(usize),
}

/// Where in the document a problem is: the steps from the top, empty for the top level.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Location(Vec<Segment>);

impl Location {
    #[must_use]
    pub fn root() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn from_segments(segments: Vec<Segment>) -> Self {
        Self(segments)
    }

    #[must_use]
    pub fn key(&self, key: Key) -> Self {
        self.step(Segment::Key(key))
    }

    #[must_use]
    pub fn item(&self, index: usize) -> Self {
        self.step(Segment::Item(index))
    }

    fn step(&self, segment: Segment) -> Self {
        let mut segments = self.0.clone();
        segments.push(segment);
        Self(segments)
    }

    #[must_use]
    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }
}

/// `ssh: port`, `default-mounts: item 0: mode`; nothing for the top level.
impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (n, segment) in self.0.iter().enumerate() {
            if n > 0 {
                f.write_str(": ")?;
            }
            match segment {
                Segment::Key(key) => f.write_str(key.as_str())?,
                Segment::Item(index) => write!(f, "item {index}")?,
            }
        }
        Ok(())
    }
}

/// A node with children in the file; fixes which keys are known inside it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Block {
    Root,
    Mount,
    Ssh,
    Timeouts,
}

impl Block {
    /// The keys the block understands, sorted.
    #[must_use]
    pub const fn known(self) -> &'static [&'static str] {
        match self {
            Self::Root => &[
                "base-nix-config",
                "default-mounts",
                "gid",
                "home-files",
                "home-storage",
                "linger",
                "ssh",
                "timeouts",
                "uid",
                "user",
            ],
            Self::Mount => &["mode", "path"],
            Self::Ssh => &["port"],
            Self::Timeouts => &["boot", "stop"],
        }
    }

    /// An example of the block as the file spells it.
    #[must_use]
    pub const fn shape(self) -> &'static str {
        match self {
            Self::Root => "nodes such as user \"alice\"",
            Self::Mount => "{ path \"/srv/app\"; mode \"rw\" }",
            Self::Ssh => "ssh { port 0 }",
            Self::Timeouts => "timeouts { boot 180; stop 60 }",
        }
    }
}

/// The type of a decoded KDL value, for messages.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ValueKind {
    Null,
    Bool,
    Int,
    Float,
    Str,
    List,
    Dict,
}

impl ValueKind {
    #[must_use]
    pub const fn of(value: &Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Bool(_) => Self::Bool,
            Value::Int(_) => Self::Int,
            Value::Float(_) => Self::Float,
            Value::Str(_) => Self::Str,
            Value::List(_) => Self::List,
            Value::Dict(_) => Self::Dict,
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool => "a boolean",
            Self::Int => "an integer",
            Self::Float => "a float",
            Self::Str => "a string",
            Self::List => "a (list) node",
            Self::Dict => "a block",
        }
    }
}

impl fmt::Display for ValueKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// What is wrong at a [`Location`].
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum ProblemKind {
    #[error("missing")]
    Missing,
    #[error("expected a string, got {found}")]
    NotString { found: ValueKind },
    #[error("expected an integer, got {found}")]
    NotInteger { found: ValueKind },
    #[error("expected a (list) node, got {found}")]
    NotList { found: ValueKind },
    #[error("expected a block, e.g. {}, got {found}", block.shape())]
    NotBlock { block: Block, found: ValueKind },
    /// The integer does not fit the field's type; `max` is the type's largest value.
    #[error("{value} is outside 0..{max}")]
    OutOfRange { value: i128, max: u32 },
    #[error("expected one of {}, got {found:?}", allowed.join(", "))]
    BadChoice {
        found: String,
        allowed: &'static [&'static str],
    },
    #[error("unknown key {key:?}; known: {}", block.known().join(", "))]
    UnknownKey { block: Block, key: String },
    #[error(transparent)]
    User(UserNameError),
    #[error(transparent)]
    Uid(UidError),
    #[error(transparent)]
    Gid(GidError),
    #[error(transparent)]
    Base(BaseSpecError),
    #[error(transparent)]
    MountPath(SpecPathError),
    #[error(transparent)]
    MountResolve(ResolveError),
    #[error(transparent)]
    HomeFile(HomeRelError),
    #[error(transparent)]
    Port(FixedPortError),
    #[error(transparent)]
    Seconds(SecondsError),
}

impl Diagnostic for ProblemKind {
    fn code(&self) -> &'static str {
        match self {
            Self::Missing => "config.missing",
            Self::NotString { .. } => "config.not-string",
            Self::NotInteger { .. } => "config.not-integer",
            Self::NotList { .. } => "config.not-list",
            Self::NotBlock { .. } => "config.not-block",
            Self::OutOfRange { .. } => "config.out-of-range",
            Self::BadChoice { .. } => "config.bad-choice",
            Self::UnknownKey { .. } => "config.unknown-key",
            Self::User(_) => "config.user",
            Self::Uid(_) => "config.uid",
            Self::Gid(_) => "config.gid",
            Self::Base(_) => "config.base",
            Self::MountPath(_) => "config.mount-path",
            Self::MountResolve(_) => "config.mount-resolve",
            Self::HomeFile(_) => "config.home-file",
            Self::Port(_) => "config.port",
            Self::Seconds(_) => "config.seconds",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::Missing => Some("add the key to the file".to_owned()),
            Self::NotString { .. } => Some("write the value as a quoted string".to_owned()),
            Self::NotInteger { .. } | Self::OutOfRange { .. } => {
                Some("write the value as a whole number".to_owned())
            }
            Self::NotList { .. } => {
                Some("write the items as a block of '-' nodes under a (list) node".to_owned())
            }
            Self::NotBlock { block, .. } => Some(format!("write it as {}", block.shape())),
            Self::BadChoice { allowed, .. } => Some(format!("use one of: {}", allowed.join(", "))),
            Self::UnknownKey { .. } => Some("remove the key, or check its spelling".to_owned()),
            Self::User(inner) => inner.hint(),
            Self::Uid(inner) => inner.hint(),
            Self::Gid(inner) => inner.hint(),
            Self::Base(inner) => inner.hint(),
            Self::MountPath(inner) => inner.hint(),
            Self::MountResolve(inner) => inner.hint(),
            Self::HomeFile(inner) => inner.hint(),
            Self::Port(inner) => inner.hint(),
            Self::Seconds(inner) => inner.hint(),
        }
    }
}

/// One thing wrong in the file: `ssh: port: bad port: ...`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Problem {
    pub at: Location,
    pub kind: ProblemKind,
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.at.is_root() {
            write!(f, "{}", self.kind)
        } else {
            write!(f, "{}: {}", self.at, self.kind)
        }
    }
}

impl std::error::Error for Problem {}

impl Diagnostic for Problem {
    fn code(&self) -> &'static str {
        self.kind.code()
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        self.kind.hint()
    }
}

/// Why a text is not a configuration.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum ConfigError {
    /// The text is not the KDL subset amoeboid reads.
    #[error(transparent)]
    Syntax(ParseError),
    /// Valid KDL, but not a valid configuration; every problem found is listed.
    #[error("invalid configuration: {}", joined(.0))]
    Invalid(NonEmpty<Problem>),
}

fn joined(problems: &NonEmpty<Problem>) -> String {
    problems
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

impl Diagnostic for ConfigError {
    fn code(&self) -> &'static str {
        match self {
            Self::Syntax(_) => "config.syntax",
            Self::Invalid(_) => "config.invalid",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some("fix the file, or delete it to start over".to_owned())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::testing::{self, Entry};

    fn kind_witness(e: &ProblemKind) -> &'static str {
        match e {
            ProblemKind::Missing => "Missing",
            ProblemKind::NotString { .. } => "NotString",
            ProblemKind::NotInteger { .. } => "NotInteger",
            ProblemKind::NotList { .. } => "NotList",
            ProblemKind::NotBlock { .. } => "NotBlock",
            ProblemKind::OutOfRange { .. } => "OutOfRange",
            ProblemKind::BadChoice { .. } => "BadChoice",
            ProblemKind::UnknownKey { .. } => "UnknownKey",
            ProblemKind::User(_) => "User",
            ProblemKind::Uid(_) => "Uid",
            ProblemKind::Gid(_) => "Gid",
            ProblemKind::Base(_) => "Base",
            ProblemKind::MountPath(_) => "MountPath",
            ProblemKind::MountResolve(_) => "MountResolve",
            ProblemKind::HomeFile(_) => "HomeFile",
            ProblemKind::Port(_) => "Port",
            ProblemKind::Seconds(_) => "Seconds",
        }
    }

    pub(crate) fn kinds() -> Vec<ProblemKind> {
        vec![
            ProblemKind::Missing,
            ProblemKind::NotString {
                found: ValueKind::Int,
            },
            ProblemKind::NotInteger {
                found: ValueKind::Bool,
            },
            ProblemKind::NotList {
                found: ValueKind::Str,
            },
            ProblemKind::NotBlock {
                block: Block::Ssh,
                found: ValueKind::Int,
            },
            ProblemKind::OutOfRange {
                value: -1,
                max: u32::MAX,
            },
            ProblemKind::BadChoice {
                found: "rx".to_owned(),
                allowed: &["rw", "ro"],
            },
            ProblemKind::UnknownKey {
                block: Block::Root,
                key: "typo".to_owned(),
            },
            ProblemKind::User(UserNameError::Root),
            ProblemKind::Uid(UidError::Root),
            ProblemKind::Gid(GidError::Root),
            ProblemKind::Base(BaseSpecError::Resolve(ResolveError::OtherUser {
                name: "bob".to_owned(),
            })),
            ProblemKind::MountPath(SpecPathError::Empty),
            ProblemKind::MountResolve(ResolveError::OtherUser {
                name: "bob".to_owned(),
            }),
            ProblemKind::HomeFile(HomeRelError::Empty),
            ProblemKind::Port(FixedPortError::Privileged { value: 22 }),
            ProblemKind::Seconds(SecondsError::Zero),
        ]
    }

    fn config_witness(e: &ConfigError) -> &'static str {
        match e {
            ConfigError::Syntax(_) => "Syntax",
            ConfigError::Invalid(_) => "Invalid",
        }
    }

    fn problem(at: Location, kind: ProblemKind) -> Problem {
        Problem { at, kind }
    }

    fn config_errors() -> Vec<ConfigError> {
        vec![
            ConfigError::Syntax(ParseError::Syntax {
                detail: "line 1".to_owned(),
            }),
            ConfigError::Invalid(NonEmpty::new(
                problem(Location::root().key(Key::Uid), ProblemKind::Missing),
                vec![problem(
                    Location::root(),
                    ProblemKind::UnknownKey {
                        block: Block::Root,
                        key: "typo".to_owned(),
                    },
                )],
            )),
        ]
    }

    /// One catalogue entry per variant (problems share their kind's codes).
    pub(crate) fn entries() -> Vec<Entry> {
        let mut all: Vec<Entry> = kinds().iter().map(Entry::of).collect();
        all.extend(config_errors().iter().map(Entry::of));
        all
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(
            &kinds(),
            kind_witness,
            &[
                "Missing",
                "NotString",
                "NotInteger",
                "NotList",
                "NotBlock",
                "OutOfRange",
                "BadChoice",
                "UnknownKey",
                "User",
                "Uid",
                "Gid",
                "Base",
                "MountPath",
                "MountResolve",
                "HomeFile",
                "Port",
                "Seconds",
            ],
        );
        testing::every_variant(&config_errors(), config_witness, &["Syntax", "Invalid"]);
    }

    #[test]
    fn keys_are_spelled_as_in_the_file() {
        testing::table(
            &[
                (Key::User, "user"),
                (Key::Uid, "uid"),
                (Key::Gid, "gid"),
                (Key::BaseNixConfig, "base-nix-config"),
                (Key::DefaultMounts, "default-mounts"),
                (Key::HomeFiles, "home-files"),
                (Key::Ssh, "ssh"),
                (Key::Port, "port"),
                (Key::Timeouts, "timeouts"),
                (Key::Boot, "boot"),
                (Key::Stop, "stop"),
                (Key::Path, "path"),
                (Key::Mode, "mode"),
                (Key::Linger, "linger"),
                (Key::HomeStorage, "home-storage"),
            ],
            |key| key.as_str(),
        );
    }

    #[test]
    fn locations_read_as_a_colon_separated_path() {
        let mount_mode = Location::from_segments(vec![
            Segment::Key(Key::DefaultMounts),
            Segment::Item(0),
            Segment::Key(Key::Mode),
        ]);
        testing::table(
            &[
                (Location::root(), String::new()),
                (Location::root().key(Key::Uid), "uid".to_owned()),
                (
                    Location::root().key(Key::Ssh).key(Key::Port),
                    "ssh: port".to_owned(),
                ),
                (
                    Location::root()
                        .key(Key::DefaultMounts)
                        .item(0)
                        .key(Key::Mode),
                    "default-mounts: item 0: mode".to_owned(),
                ),
            ],
            ToString::to_string,
        );
        assert_eq!(
            Location::root()
                .key(Key::DefaultMounts)
                .item(0)
                .key(Key::Mode),
            mount_mode
        );
        assert!(Location::root().is_root());
        assert!(!Location::root().item(1).is_root());
    }

    #[test]
    fn known_keys_of_every_block_are_sorted_and_shapes_are_written() {
        for block in [Block::Root, Block::Mount, Block::Ssh, Block::Timeouts] {
            let mut sorted = block.known().to_vec();
            sorted.sort_unstable();
            assert_eq!(block.known(), sorted.as_slice(), "{block:?}");
            assert!(!block.shape().is_empty(), "{block:?}");
        }
    }

    #[test]
    fn value_kinds_are_taken_from_values_and_labelled() {
        use crate::kdl::fixtures;
        testing::table(
            &[
                (Value::Null, (ValueKind::Null, "null")),
                (Value::Bool(true), (ValueKind::Bool, "a boolean")),
                (Value::Int(fixtures::int(1)), (ValueKind::Int, "an integer")),
                (
                    Value::Float(crate::kdl::Float::new(1.5)),
                    (ValueKind::Float, "a float"),
                ),
                (Value::Str(String::new()), (ValueKind::Str, "a string")),
                (Value::List(vec![]), (ValueKind::List, "a (list) node")),
                (
                    Value::Dict(crate::kdl::Document::new()),
                    (ValueKind::Dict, "a block"),
                ),
            ],
            |value| (ValueKind::of(value), ValueKind::of(value).label()),
        );
        assert_eq!(ValueKind::Int.to_string(), "an integer");
    }

    #[test]
    fn problem_messages_name_the_location_unless_it_is_the_top_level() {
        let cases = [
            (
                problem(Location::root().key(Key::Uid), ProblemKind::Missing),
                "uid: missing",
            ),
            (
                problem(
                    Location::root(),
                    ProblemKind::UnknownKey {
                        block: Block::Timeouts,
                        key: "kill".to_owned(),
                    },
                ),
                "unknown key \"kill\"; known: boot, stop",
            ),
        ];
        for (n, (problem, message)) in cases.iter().enumerate() {
            assert_eq!(problem.message(), *message, "case {n}");
        }
    }

    #[test]
    fn a_problem_has_the_code_and_hint_of_its_kind() {
        for kind in kinds() {
            let wrapped = problem(Location::root().key(Key::Uid), kind.clone());
            assert_eq!(wrapped.code(), kind.code());
            assert_eq!(wrapped.hint(), kind.hint());
        }
    }

    #[test]
    fn kind_messages_and_hints_are_written_out() {
        let cases = [
            (ProblemKind::Missing, ("missing", "add the key to the file")),
            (
                ProblemKind::NotString {
                    found: ValueKind::Int,
                },
                (
                    "expected a string, got an integer",
                    "write the value as a quoted string",
                ),
            ),
            (
                ProblemKind::NotInteger {
                    found: ValueKind::Bool,
                },
                (
                    "expected an integer, got a boolean",
                    "write the value as a whole number",
                ),
            ),
            (
                ProblemKind::NotList {
                    found: ValueKind::Str,
                },
                (
                    "expected a (list) node, got a string",
                    "write the items as a block of '-' nodes under a (list) node",
                ),
            ),
            (
                ProblemKind::NotBlock {
                    block: Block::Ssh,
                    found: ValueKind::Int,
                },
                (
                    "expected a block, e.g. ssh { port 0 }, got an integer",
                    "write it as ssh { port 0 }",
                ),
            ),
            (
                ProblemKind::OutOfRange {
                    value: -1,
                    max: 65_535,
                },
                (
                    "-1 is outside 0..65535",
                    "write the value as a whole number",
                ),
            ),
            (
                ProblemKind::BadChoice {
                    found: "rx".to_owned(),
                    allowed: &["rw", "ro"],
                },
                ("expected one of rw, ro, got \"rx\"", "use one of: rw, ro"),
            ),
            (
                ProblemKind::UnknownKey {
                    block: Block::Ssh,
                    key: "colour".to_owned(),
                },
                (
                    "unknown key \"colour\"; known: port",
                    "remove the key, or check its spelling",
                ),
            ),
        ];
        for (n, (kind, (message, hint))) in cases.iter().enumerate() {
            assert_eq!(kind.message(), *message, "case {n}");
            assert_eq!(kind.hint().as_deref(), Some(*hint), "case {n}");
        }
    }

    #[test]
    fn wrapped_domain_errors_keep_their_message_and_hint() {
        let inner = UserNameError::Root;
        let wrapped = ProblemKind::User(inner.clone());
        assert_eq!(wrapped.message(), inner.message());
        assert_eq!(wrapped.hint(), inner.hint());
    }

    #[test]
    fn an_invalid_configuration_lists_every_problem_on_one_line() {
        let errors = config_errors();
        assert_eq!(
            errors.get(1).map(Diagnostic::message),
            Some(
                "invalid configuration: uid: missing; unknown key \"typo\"; known: base-nix-config, \
                 default-mounts, gid, home-files, home-storage, linger, ssh, timeouts, uid, user"
                    .to_owned()
            )
        );
        assert_eq!(
            errors.first().map(Diagnostic::message),
            Some("invalid KDL: line 1".to_owned())
        );
    }
}
