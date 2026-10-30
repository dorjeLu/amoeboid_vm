//! `SpecPath`: a path as the operator typed it (relative, `~`), and its lexical resolution.

use crate::diagnostic::Diagnostic;
use crate::domain::abs_path::{AbsDir, HomeDir, Segment, sealed};
use crate::refined::{Refined, Refinement};

/// Marker of [`SpecPath`].
pub struct SpecPathShape;

/// Non-empty path text without NUL, not yet resolved against a directory.
pub type SpecPath = Refined<String, SpecPathShape>;

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum SpecPathError {
    #[error("empty path")]
    Empty,
    /// `at` is a byte offset.
    #[error("NUL byte in path at offset {at}")]
    NulByte { at: usize },
}

impl Diagnostic for SpecPathError {
    fn code(&self) -> &'static str {
        match self {
            Self::Empty => "spec-path.empty",
            Self::NulByte { .. } => "spec-path.nul-byte",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some("give a directory such as ~/dev/app, ./app or /srv/app".to_owned())
    }
}

impl Refinement<String> for SpecPathShape {
    type Error = SpecPathError;

    fn check(value: &String) -> Result<(), SpecPathError> {
        if value.is_empty() {
            return Err(SpecPathError::Empty);
        }
        match value.find('\0') {
            Some(at) => Err(SpecPathError::NulByte { at }),
            None => Ok(()),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum ResolveError {
    /// `~name/...`: another account's home is not known without the user database.
    #[error("can't expand ~{name}: only ~ and ~/... are supported")]
    OtherUser { name: String },
}

impl Diagnostic for ResolveError {
    fn code(&self) -> &'static str {
        match self {
            Self::OtherUser { .. } => "resolve.other-user",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::OtherUser { .. } => {
                Some("write the path in full, or start it with ./".to_owned())
            }
        }
    }
}

impl SpecPath {
    /// The absolute, normalised directory this text names: `~` and `~/...` are the invoking
    /// user's `home`, other relative text is under `cwd`; `.`, `..` and repeated `/` are folded
    /// textually, `..` at the root stays at the root.
    ///
    /// Symlinks are not looked at: a `..` after a symlink differs from what the kernel does.
    /// Observing the directory (`HostDir`) resolves them.
    ///
    /// # Errors
    ///
    /// `~name` (another account's home) is refused.
    pub fn resolve(&self, cwd: &AbsDir, home: &HomeDir) -> Result<AbsDir, ResolveError> {
        let raw = self.get().as_str();
        let (start, rest) = if raw == "~" {
            (home.to_dir(), "")
        } else if let Some(below) = raw.strip_prefix("~/") {
            (home.to_dir(), below)
        } else if let Some(rest) = raw.strip_prefix('~') {
            let name = rest.split('/').next().unwrap_or_default();
            return Err(ResolveError::OtherUser {
                name: name.to_owned(),
            });
        } else if let Some(below) = raw.strip_prefix('/') {
            (AbsDir::root(), below)
        } else {
            (cwd.clone(), raw)
        };
        Ok(rest.split('/').fold(start, |dir, part| match part {
            "" | "." => dir,
            ".." => dir.parent(),
            name => dir.join_dir(&Part(name)),
        }))
    }
}

/// A component of a [`SpecPath`] other than empty, `.` and `..`: split at `/`, and NUL-free
/// because the whole text is.
struct Part<'a>(&'a str);

impl sealed::Sealed for Part<'_> {}

impl Segment for Part<'_> {
    fn segment(&self) -> &str {
        self.0
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::testing::{self, Entry};

    fn spec_witness(e: &SpecPathError) -> &'static str {
        match e {
            SpecPathError::Empty => "Empty",
            SpecPathError::NulByte { .. } => "NulByte",
        }
    }

    fn resolve_witness(e: &ResolveError) -> &'static str {
        match e {
            ResolveError::OtherUser { .. } => "OtherUser",
        }
    }

    fn spec_errors() -> Vec<SpecPathError> {
        vec![SpecPathError::Empty, SpecPathError::NulByte { at: 2 }]
    }

    fn resolve_errors() -> Vec<ResolveError> {
        vec![ResolveError::OtherUser {
            name: "bob".to_owned(),
        }]
    }

    /// One catalogue entry per variant of both enums.
    pub(crate) fn entries() -> Vec<Entry> {
        let specs = spec_errors().iter().map(Entry::of).collect::<Vec<_>>();
        let resolves = resolve_errors().iter().map(Entry::of).collect::<Vec<_>>();
        [specs, resolves].concat()
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(&spec_errors(), spec_witness, &["Empty", "NulByte"]);
        testing::every_variant(&resolve_errors(), resolve_witness, &["OtherUser"]);
    }

    #[test]
    fn spec_paths_are_any_non_empty_text_without_nul() {
        testing::table(
            &[
                ("", Err(SpecPathError::Empty)),
                ("a", Ok(())),
                ("/", Ok(())),
                ("~", Ok(())),
                ("../x//y/./", Ok(())),
                ("a b\n日本", Ok(())),
                ("\0", Err(SpecPathError::NulByte { at: 0 })),
                ("é\0", Err(SpecPathError::NulByte { at: 2 })),
            ],
            |text| SpecPath::parse((*text).to_owned()).map(|_| ()),
        );
    }

    fn resolved(text: &str) -> Result<String, ResolveError> {
        let cwd = AbsDir::parse("/work".to_owned()).unwrap();
        let home = HomeDir::parse("/home/u".to_owned()).unwrap();
        let result = SpecPath::parse(text.to_owned())
            .unwrap()
            .resolve(&cwd, &home);
        // Every result is what parsing its text would give: the total resolution keeps the proof.
        if let Ok(dir) = &result {
            assert_eq!(
                AbsDir::parse(dir.get().clone()),
                Ok(dir.clone()),
                "{text:?}"
            );
        }
        result.map(AbsDir::into_inner)
    }

    #[allow(clippy::unnecessary_wraps, reason = "a table cell is a Result")]
    fn ok(text: &str) -> Result<String, ResolveError> {
        Ok(text.to_owned())
    }

    fn other_user(name: &str) -> Result<String, ResolveError> {
        Err(ResolveError::OtherUser {
            name: name.to_owned(),
        })
    }

    #[test]
    fn spec_paths_resolve_against_cwd_and_home_with_dots_folded() {
        testing::table(
            &[
                ("/srv/data", ok("/srv/data")),
                ("rel/dir", ok("/work/rel/dir")),
                ("/srv/../srv/x/", ok("/srv/x")),
                ("a/../b", ok("/work/b")),
                (".", ok("/work")),
                ("./a", ok("/work/a")),
                ("..", ok("/")),
                ("../../..", ok("/")),
                ("/..", ok("/")),
                ("/", ok("/")),
                ("//a///b", ok("/a/b")),
                ("/odd:name", ok("/odd:name")),
                ("~", ok("/home/u")),
                ("~/", ok("/home/u")),
                ("~/x", ok("/home/u/x")),
                ("~/x/../y/", ok("/home/u/y")),
                ("~/..", ok("/home")),
                ("~root", other_user("root")),
                ("~bob/x", other_user("bob")),
                ("./~", ok("/work/~")),
                ("a/~", ok("/work/a/~")),
                ("...", ok("/work/...")),
                ("a/..b/.c", ok("/work/a/..b/.c")),
                ("a b\n日本", ok("/work/a b\n日本")),
            ],
            |text| resolved(text),
        );
    }

    #[test]
    fn a_root_home_resolves_below_it_without_doubling_the_slash() {
        let cwd = AbsDir::parse("/".to_owned()).unwrap();
        let home = HomeDir::parse("/".to_owned()).unwrap();
        let path = SpecPath::parse("~/x".to_owned()).unwrap();
        assert_eq!(
            path.resolve(&cwd, &home),
            Ok(AbsDir::parse("/x".to_owned()).unwrap())
        );
    }
}
