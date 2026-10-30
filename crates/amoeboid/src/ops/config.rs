//! Loading, creating and saving `run_config.kdl` on a [`HostFs`]. The file is read once per
//! command and written back only whole, atomically, in canonical form.

use crate::config::{self, Anchor, ConfigError, RunConfig};
use crate::diagnostic::Diagnostic;
use crate::domain::{AbsPath, FileMode, GuestRel, HomeRel, Identity, Observation};
use crate::layout::Paths;
use crate::ports::host_fs::{FsError, HostFs};
use crate::refined;

/// Home files a fresh configuration lists when they exist: the usual git settings.
fn home_file_candidates() -> [HomeRel; 2] {
    [
        refined!(HomeRel, ".gitconfig"),
        refined!(HomeRel, ".config/git/config"),
    ]
}

/// What went wrong with the configuration file (`file` is always in the message).
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum ConfigFileError {
    #[error(transparent)]
    Read(FsError),
    #[error("{file}: the file isn't UTF-8 text")]
    NotUtf8 { file: AbsPath },
    #[error("{file}: {error}")]
    Invalid {
        file: AbsPath,
        error: Box<ConfigError>,
    },
    /// Looking whether the file (or a candidate home file) exists.
    #[error(transparent)]
    Probe(FsError),
    #[error(transparent)]
    Write(FsError),
}

impl Diagnostic for ConfigFileError {
    fn code(&self) -> &'static str {
        match self {
            Self::Read(_) => "config-file.read",
            Self::NotUtf8 { .. } => "config-file.not-utf8",
            Self::Invalid { .. } => "config-file.invalid",
            Self::Probe(_) => "config-file.probe",
            Self::Write(_) => "config-file.write",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::Read(error) | Self::Probe(error) | Self::Write(error) => error.hint(),
            Self::NotUtf8 { .. } => Some("fix the file, or delete it to start over".to_owned()),
            Self::Invalid { error, .. } => error.hint(),
        }
    }
}

/// Whether [`load_or_create`] found the file or wrote a fresh one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Origin {
    Found,
    Created,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Loaded {
    pub config: RunConfig,
    pub origin: Origin,
}

fn anchor(paths: &Paths) -> Anchor {
    Anchor {
        config_dir: paths.config_dir().clone(),
        home: paths.home().clone(),
    }
}

/// Reads and decodes the configuration file; relative paths in it are under the config dir.
///
/// # Errors
///
/// The file can't be read, isn't text, or isn't a valid configuration.
pub async fn load(fs: &impl HostFs, paths: &Paths) -> Result<RunConfig, ConfigFileError> {
    let file = paths.config_file();
    let bytes = fs.read_file(&file).await.map_err(ConfigFileError::Read)?;
    let Ok(text) = String::from_utf8(bytes) else {
        return Err(ConfigFileError::NotUtf8 { file });
    };
    config::parse(&text, &anchor(paths)).map_err(|error| ConfigFileError::Invalid {
        file,
        error: Box::new(error),
    })
}

/// Replaces the configuration file with `config` in canonical form (header comment included):
/// readers see the old file or the new one. Makes the config directory when it is missing.
///
/// # Errors
///
/// The directory or the file can't be written.
pub async fn save(
    fs: &impl HostFs,
    paths: &Paths,
    config: &RunConfig,
) -> Result<(), ConfigFileError> {
    fs.create_dir_all(paths.config_dir(), refined!(FileMode, 0o755))
        .await
        .map_err(ConfigFileError::Write)?;
    fs.write_file(
        &paths.config_file(),
        config::render(config).into_bytes(),
        refined!(FileMode, 0o644),
    )
    .await
    .map_err(ConfigFileError::Write)
}

/// A fresh configuration for `identity`: defaults, no mounts (the user chooses), and those of the
/// usual home files that exist.
async fn initial(
    fs: &impl HostFs,
    paths: &Paths,
    identity: Identity,
) -> Result<RunConfig, ConfigFileError> {
    let mut config = RunConfig::new(identity.user, identity.uid, identity.gid);
    for rel in home_file_candidates() {
        let path = paths.home().join_rel(&GuestRel::of_home(&rel));
        match fs.observe(&path).await.map_err(ConfigFileError::Probe)? {
            Observation::File { .. } => config.home_files.push(rel),
            Observation::Missing | Observation::Directory { .. } | Observation::Other => {}
        }
    }
    Ok(config)
}

/// Loads the configuration, writing a fresh one on first run (when nothing is at the path).
///
/// # Errors
///
/// As [`load`] and [`save`], or a probe task that did not finish.
pub async fn load_or_create(
    fs: &impl HostFs,
    paths: &Paths,
    identity: Identity,
) -> Result<Loaded, ConfigFileError> {
    match fs
        .observe(&paths.config_file())
        .await
        .map_err(ConfigFileError::Probe)?
    {
        Observation::Missing => {
            let config = initial(fs, paths, identity).await?;
            save(fs, paths, &config).await?;
            Ok(Loaded {
                config,
                origin: Origin::Created,
            })
        }
        Observation::File { .. } | Observation::Directory { .. } | Observation::Other => {
            Ok(Loaded {
                config: load(fs, paths).await?,
                origin: Origin::Found,
            })
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(
    clippy::disallowed_types,
    reason = "tests plant files on a real filesystem"
)]
pub(crate) mod tests {
    use std::fs;
    use std::os::unix::fs::{MetadataExt as _, symlink};

    use super::*;
    use crate::adapters::StdFs;
    use crate::domain::{AbsDir, Uid, UserName};
    use crate::layout::{Env, Var};
    use crate::testing::{self, Entry, TempDir};

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
    }

    fn identity() -> Identity {
        Identity::parse("alice".to_owned(), 1000, 100).unwrap()
    }

    /// A home, config and data dir inside `temp`.
    fn paths(temp: &TempDir) -> Paths {
        let root = temp.path().to_str().unwrap();
        let var = |dir: &str| Var::Text(format!("{root}/{dir}"));
        Paths::resolve(
            &Env {
                home: var("home"),
                xdg_config_home: var("cfg"),
                xdg_data_home: var("data"),
                xdg_runtime_dir: var("run"),
                tmpdir: Var::Unset,
            },
            Uid::parse(1000).unwrap(),
        )
        .unwrap()
    }

    fn raw(path: &AbsPath) -> &std::path::Path {
        std::path::Path::new(path.get())
    }

    fn put_home(temp: &TempDir, rel: &str, bytes: &[u8]) {
        let file = temp.path().join("home").join(rel);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, bytes).unwrap();
    }

    fn put_config(temp: &TempDir, paths: &Paths, bytes: &[u8]) {
        let _ = temp;
        fs::create_dir_all(raw(&paths.config_file()).parent().unwrap()).unwrap();
        fs::write(raw(&paths.config_file()), bytes).unwrap();
    }

    fn config_error() -> ConfigError {
        let anchor = Anchor {
            config_dir: AbsDir::parse("/cfg".to_owned()).unwrap(),
            home: crate::domain::HomeDir::parse("/home/alice".to_owned()).unwrap(),
        };
        config::parse("user 1\n", &anchor).unwrap_err()
    }

    fn fs_error() -> FsError {
        FsError::Aborted {
            op: crate::ports::host_fs::FsOp::Read,
            path: AbsPath::parse("/cfg/run_config.kdl".to_owned()).unwrap(),
        }
    }

    fn witness(e: &ConfigFileError) -> &'static str {
        match e {
            ConfigFileError::Read(_) => "Read",
            ConfigFileError::NotUtf8 { .. } => "NotUtf8",
            ConfigFileError::Invalid { .. } => "Invalid",
            ConfigFileError::Probe(_) => "Probe",
            ConfigFileError::Write(_) => "Write",
        }
    }

    fn errors() -> Vec<ConfigFileError> {
        let file = AbsPath::parse("/cfg/run_config.kdl".to_owned()).unwrap();
        vec![
            ConfigFileError::Read(fs_error()),
            ConfigFileError::NotUtf8 { file: file.clone() },
            ConfigFileError::Invalid {
                file,
                error: Box::new(config_error()),
            },
            ConfigFileError::Probe(fs_error()),
            ConfigFileError::Write(fs_error()),
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
            &["Read", "NotUtf8", "Invalid", "Probe", "Write"],
        );
    }

    #[test]
    fn the_home_file_candidates_are_the_two_git_configs() {
        let names: Vec<String> = home_file_candidates()
            .iter()
            .map(|rel| rel.get().clone())
            .collect();
        assert_eq!(names, [".gitconfig", ".config/git/config"]);
    }

    #[test]
    fn a_saved_configuration_loads_back_equal_and_the_file_has_the_header() {
        let temp = TempDir::new("cfg-roundtrip").unwrap();
        let paths = paths(&temp);
        let mut config = RunConfig::new(
            UserName::parse("alice".to_owned()).unwrap(),
            Uid::parse(1000).unwrap(),
            crate::domain::Gid::parse(100).unwrap(),
        );
        config.home_files = home_file_candidates().to_vec();
        runtime().block_on(async {
            save(&StdFs, &paths, &config).await.unwrap();
            assert_eq!(load(&StdFs, &paths).await, Ok(config.clone()));
        });
        let text = fs::read_to_string(raw(&paths.config_file())).unwrap();
        assert_eq!(text, config::render(&config));
        assert!(text.starts_with(config::HEADER));
        let meta = fs::metadata(raw(&paths.config_file())).unwrap();
        assert_eq!(meta.mode() & 0o7777, 0o644);
    }

    #[test]
    fn saving_replaces_the_file_whole_and_leaves_no_temp_file() {
        let temp = TempDir::new("cfg-replace").unwrap();
        let paths = paths(&temp);
        put_config(&temp, &paths, b"stale garbage");
        let config = RunConfig::new(
            UserName::parse("alice".to_owned()).unwrap(),
            Uid::parse(1000).unwrap(),
            crate::domain::Gid::parse(100).unwrap(),
        );
        runtime().block_on(async {
            save(&StdFs, &paths, &config).await.unwrap();
        });
        let dir = raw(&paths.config_file()).parent().unwrap().to_owned();
        let names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, ["run_config.kdl"]);
        assert_eq!(
            fs::read_to_string(raw(&paths.config_file())).unwrap(),
            config::render(&config)
        );
    }

    #[test]
    fn saving_into_a_config_dir_that_is_a_file_is_a_write_error_naming_the_path() {
        let temp = TempDir::new("cfg-save-fail").unwrap();
        let paths = paths(&temp);
        fs::write(temp.path().join("cfg"), b"not a dir").unwrap();
        let config = RunConfig::new(
            UserName::parse("alice".to_owned()).unwrap(),
            Uid::parse(1000).unwrap(),
            crate::domain::Gid::parse(100).unwrap(),
        );
        let error = runtime()
            .block_on(save(&StdFs, &paths, &config))
            .unwrap_err();
        assert!(matches!(error, ConfigFileError::Write(_)), "{error:?}");
        assert!(error.message().contains("/cfg"), "{}", error.message());
    }

    #[test]
    fn loading_a_missing_file_is_a_read_error_naming_it() {
        let temp = TempDir::new("cfg-missing").unwrap();
        let paths = paths(&temp);
        let error = runtime().block_on(load(&StdFs, &paths)).unwrap_err();
        assert!(matches!(error, ConfigFileError::Read(_)), "{error:?}");
        assert!(
            error.message().contains("cfg/amoeboid/run_config.kdl"),
            "{}",
            error.message()
        );
    }

    #[test]
    fn a_file_that_is_not_utf8_is_refused_with_its_path() {
        let temp = TempDir::new("cfg-bytes").unwrap();
        let paths = paths(&temp);
        put_config(&temp, &paths, b"user \xff");
        let error = runtime().block_on(load(&StdFs, &paths)).unwrap_err();
        assert_eq!(
            error,
            ConfigFileError::NotUtf8 {
                file: paths.config_file()
            }
        );
    }

    #[test]
    fn invalid_and_unparsable_files_carry_the_path_and_every_problem() {
        let temp = TempDir::new("cfg-invalid");
        let temp = temp.unwrap();
        let paths = paths(&temp);
        put_config(&temp, &paths, b"user \"Bad Name\"\nuid 0\n");
        let error = runtime().block_on(load(&StdFs, &paths)).unwrap_err();
        assert!(
            matches!(
                &error,
                ConfigFileError::Invalid { file, error }
                    if file == &paths.config_file()
                        && matches!(&**error, ConfigError::Invalid(p) if p.len().get() >= 3)
            ),
            "{error:?}"
        );
        assert!(error.message().starts_with(paths.config_file().get()));
        put_config(&temp, &paths, b"user {");
        let error = runtime().block_on(load(&StdFs, &paths)).unwrap_err();
        assert!(
            matches!(
                &error,
                ConfigFileError::Invalid { error, .. }
                    if matches!(**error, ConfigError::Syntax(_))
            ),
            "{error:?}"
        );
    }

    #[test]
    fn relative_paths_in_the_file_are_under_the_config_dir() {
        let temp = TempDir::new("cfg-anchor").unwrap();
        let paths = paths(&temp);
        put_config(
            &temp,
            &paths,
            b"user alice\nuid 1000\ngid 100\n(list)default-mounts {\n  - {\n    path proj\n    mode rw\n  }\n}\n",
        );
        let config = runtime().block_on(load(&StdFs, &paths)).unwrap();
        let dirs: Vec<&str> = config
            .default_mounts
            .iter()
            .map(|m| m.path.get().as_str())
            .collect();
        assert_eq!(
            dirs,
            [format!("{}/proj", paths.config_dir().get()).as_str()]
        );
    }

    #[test]
    fn a_first_run_writes_a_fresh_configuration_listing_the_home_files_that_exist() {
        let temp = TempDir::new("cfg-first").unwrap();
        let paths = paths(&temp);
        put_home(&temp, ".gitconfig", b"[user]");
        fs::create_dir_all(temp.path().join("home/.config/git/config")).unwrap();
        let loaded = runtime()
            .block_on(load_or_create(&StdFs, &paths, identity()))
            .unwrap();
        let mut expected = RunConfig::new(identity().user, identity().uid, identity().gid);
        expected.home_files = vec![refined!(HomeRel, ".gitconfig")];
        assert_eq!(
            loaded,
            Loaded {
                config: expected.clone(),
                origin: Origin::Created
            }
        );
        assert_eq!(runtime().block_on(load(&StdFs, &paths)), Ok(expected));
    }

    #[test]
    fn a_symlink_to_a_file_counts_as_a_home_file_and_a_dangling_one_does_not() {
        let temp = TempDir::new("cfg-links").unwrap();
        let paths = paths(&temp);
        put_home(&temp, "dotfiles/git", b"[user]");
        fs::create_dir_all(temp.path().join("home/.config/git")).unwrap();
        symlink(
            temp.path().join("home/dotfiles/git"),
            temp.path().join("home/.config/git/config"),
        )
        .unwrap();
        symlink("/nonexistent/x", temp.path().join("home/.gitconfig")).unwrap();
        let loaded = runtime()
            .block_on(load_or_create(&StdFs, &paths, identity()))
            .unwrap();
        assert_eq!(
            loaded.config.home_files,
            vec![refined!(HomeRel, ".config/git/config")]
        );
    }

    #[test]
    fn an_existing_file_is_loaded_and_never_overwritten_even_when_invalid() {
        let temp = TempDir::new("cfg-keep").unwrap();
        let paths = paths(&temp);
        put_config(&temp, &paths, b"user alice\nuid 1000\ngid 100\n");
        let loaded = runtime()
            .block_on(load_or_create(&StdFs, &paths, identity()))
            .unwrap();
        assert_eq!(loaded.origin, Origin::Found);
        assert_eq!(loaded.config.home_files, Vec::new());
        put_config(&temp, &paths, b"nonsense {");
        let error = runtime()
            .block_on(load_or_create(&StdFs, &paths, identity()))
            .unwrap_err();
        assert!(
            matches!(error, ConfigFileError::Invalid { .. }),
            "{error:?}"
        );
        assert_eq!(fs::read(raw(&paths.config_file())).unwrap(), b"nonsense {");
    }

    #[test]
    fn a_directory_at_the_config_path_is_a_read_error_not_a_first_run() {
        let temp = TempDir::new("cfg-dir").unwrap();
        let paths = paths(&temp);
        fs::create_dir_all(raw(&paths.config_file())).unwrap();
        let error = runtime()
            .block_on(load_or_create(&StdFs, &paths, identity()))
            .unwrap_err();
        assert!(matches!(error, ConfigFileError::Read(_)), "{error:?}");
    }

    #[test]
    fn hints_forward_the_inner_remedy() {
        let hints: Vec<Option<String>> = errors().iter().map(Diagnostic::hint).collect();
        assert_eq!(hints.len(), 5);
        assert_eq!(
            hints.get(1).and_then(Option::as_deref),
            Some("fix the file, or delete it to start over")
        );
        assert_eq!(
            hints.get(2).and_then(Option::as_deref),
            Some("fix the file, or delete it to start over")
        );
        assert_eq!(hints.first(), Some(&fs_error().hint()));
    }
}
