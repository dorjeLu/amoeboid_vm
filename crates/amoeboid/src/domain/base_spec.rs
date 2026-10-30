//! `BaseSpec`: what `--base-nix-config` (or the `base-nix-config` key) names: a preset or a
//! directory.

use crate::diagnostic::Diagnostic;
use crate::domain::abs_path::{AbsDir, HomeDir};
use crate::domain::preset_name::{PresetName, PresetNameError};
use crate::domain::spec_path::{ResolveError, SpecPath, SpecPathError};

/// A bare name is a preset; text that looks like a path is a directory, made absolute.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum BaseSpec {
    Preset(PresetName),
    Dir(AbsDir),
}

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum BaseSpecError {
    #[error(transparent)]
    Preset(PresetNameError),
    #[error(transparent)]
    Path(SpecPathError),
    #[error(transparent)]
    Resolve(ResolveError),
}

impl Diagnostic for BaseSpecError {
    fn code(&self) -> &'static str {
        match self {
            Self::Preset(_) => "base-spec.preset",
            Self::Path(_) => "base-spec.path",
            Self::Resolve(_) => "base-spec.resolve",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::Preset(inner) => inner.hint(),
            Self::Path(inner) => inner.hint(),
            Self::Resolve(inner) => inner.hint(),
        }
    }
}

impl BaseSpec {
    /// Text with a `/`, or starting with `~` or `.`, is a path, not a preset name.
    #[must_use]
    pub fn is_path_like(spec: &str) -> bool {
        spec.contains('/') || spec.starts_with(['~', '.'])
    }

    /// `cwd` anchors relative paths; `home` is what `~` means.
    ///
    /// # Errors
    ///
    /// A bare name that is not a preset name, or a path that cannot be resolved.
    pub fn parse(spec: &str, cwd: &AbsDir, home: &HomeDir) -> Result<Self, BaseSpecError> {
        if !Self::is_path_like(spec) {
            return PresetName::parse(spec.to_owned())
                .map(Self::Preset)
                .map_err(BaseSpecError::Preset);
        }
        SpecPath::parse(spec.to_owned())
            .map_err(BaseSpecError::Path)?
            .resolve(cwd, home)
            .map(Self::Dir)
            .map_err(BaseSpecError::Resolve)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::testing::{self, Entry};

    fn witness(e: &BaseSpecError) -> &'static str {
        match e {
            BaseSpecError::Preset(_) => "Preset",
            BaseSpecError::Path(_) => "Path",
            BaseSpecError::Resolve(_) => "Resolve",
        }
    }

    fn errors() -> Vec<BaseSpecError> {
        vec![
            BaseSpecError::Preset(PresetNameError::Empty),
            BaseSpecError::Path(SpecPathError::NulByte { at: 1 }),
            BaseSpecError::Resolve(ResolveError::OtherUser {
                name: "bob".to_owned(),
            }),
        ]
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        errors().iter().map(Entry::of).collect()
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(&errors(), witness, &["Preset", "Path", "Resolve"]);
    }

    #[test]
    fn a_wrapped_error_keeps_the_message_and_hint_of_its_cause() {
        for error in errors() {
            let (message, hint) = match &error {
                BaseSpecError::Preset(inner) => (inner.message(), inner.hint()),
                BaseSpecError::Path(inner) => (inner.message(), inner.hint()),
                BaseSpecError::Resolve(inner) => (inner.message(), inner.hint()),
            };
            assert_eq!((error.message(), error.hint()), (message, hint));
        }
    }

    fn base(spec: &str) -> Result<BaseSpec, BaseSpecError> {
        let cwd = AbsDir::parse("/work".to_owned()).unwrap();
        let home = HomeDir::parse("/home/u".to_owned()).unwrap();
        BaseSpec::parse(spec, &cwd, &home)
    }

    #[allow(clippy::unnecessary_wraps, reason = "a table cell is a Result")]
    fn preset(name: &str) -> Result<BaseSpec, BaseSpecError> {
        Ok(BaseSpec::Preset(
            PresetName::parse(name.to_owned()).unwrap(),
        ))
    }

    #[allow(clippy::unnecessary_wraps, reason = "a table cell is a Result")]
    fn dir(path: &str) -> Result<BaseSpec, BaseSpecError> {
        Ok(BaseSpec::Dir(AbsDir::parse(path.to_owned()).unwrap()))
    }

    #[test]
    fn path_like_specs_are_those_with_a_slash_or_starting_with_tilde_or_dot() {
        testing::table(
            &[
                ("default", false),
                ("proj-1", false),
                ("A9", false),
                ("a.b", false),
                ("", false),
                ("a/b", true),
                ("/abs", true),
                ("./x", true),
                ("../x", true),
                (".", true),
                ("..", true),
                (".hidden", true),
                ("~", true),
                ("~/x", true),
                ("~user", true),
                ("a~", false),
            ],
            |text| BaseSpec::is_path_like(text),
        );
    }

    #[test]
    fn base_specs_are_presets_or_resolved_directories() {
        testing::table(
            &[
                ("default", preset("default")),
                ("my.preset_2", preset("my.preset_2")),
                ("", Err(BaseSpecError::Preset(PresetNameError::Empty))),
                (
                    "-x",
                    Err(BaseSpecError::Preset(PresetNameError::BadStart { ch: '-' })),
                ),
                (
                    "a b",
                    Err(BaseSpecError::Preset(PresetNameError::BadChar {
                        ch: ' ',
                        at: 1,
                    })),
                ),
                ("/opt/nix", dir("/opt/nix")),
                ("rel/nix", dir("/work/rel/nix")),
                ("./nix", dir("/work/nix")),
                (".", dir("/work")),
                ("../nix/", dir("/nix")),
                (".hidden", dir("/work/.hidden")),
                ("~/nix", dir("/home/u/nix")),
                ("~", dir("/home/u")),
                (
                    "~bob/nix",
                    Err(BaseSpecError::Resolve(ResolveError::OtherUser {
                        name: "bob".to_owned(),
                    })),
                ),
                (
                    "./a\0",
                    Err(BaseSpecError::Path(SpecPathError::NulByte { at: 3 })),
                ),
            ],
            |text| base(text),
        );
    }
}
