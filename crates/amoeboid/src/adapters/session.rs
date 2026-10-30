//! `StdSession`: [`Session`] on systemd's per-user marker files and `loginctl`.

use std::future::Future;
use std::process::Stdio;

use tokio::process::Command;

use crate::domain::{AbsDir, AbsPath, FileName, Linger, UserName};
use crate::plan::loginctl_argv;
use crate::ports::Cause;
use crate::ports::session::{Session, SessionError};
use crate::refined;

const SYSTEMD: FileName = refined!(FileName, "systemd");
const LIB: FileName = refined!(FileName, "lib");
const VAR: FileName = refined!(FileName, "var");
const LINGER: FileName = refined!(FileName, "linger");

/// Linger state is a file named after the user in systemd-logind's state directory.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct StdSession {
    linger_dir: AbsDir,
}

impl Default for StdSession {
    fn default() -> Self {
        Self::at(
            AbsDir::root()
                .join_dir(&VAR)
                .join_dir(&LIB)
                .join_dir(&SYSTEMD)
                .join_dir(&LINGER),
        )
    }
}

impl StdSession {
    /// Reads the markers in `linger_dir`; for tests with a fake one.
    #[must_use]
    pub const fn at(linger_dir: AbsDir) -> Self {
        Self { linger_dir }
    }

    fn marker(&self, user: &UserName) -> AbsPath {
        self.linger_dir.join(user)
    }
}

impl Session for StdSession {
    fn linger(&self, user: &UserName) -> impl Future<Output = Result<Linger, SessionError>> + Send {
        let marker = self.marker(user);
        async move {
            tokio::task::spawn_blocking(move || Linger::observe(&super::fs::observe(&marker)))
                .await
                .map_err(|_| SessionError::Aborted)
        }
    }

    fn enable_linger(
        &self,
        user: &UserName,
    ) -> impl Future<Output = Result<(), SessionError>> + Send {
        let argv = loginctl_argv(user);
        async move { run(argv.program(), argv.args()).await }
    }
}

/// Runs `program` without a terminal and reads its exit status.
async fn run(program: &str, args: &[String]) -> Result<(), SessionError> {
    let status = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .status()
        .await
        .map_err(|error| SessionError::Spawn {
            cause: Cause::of(&error),
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(SessionError::Failed {
            code: status.code(),
        })
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::fs;
    use std::io;
    use std::os::unix::fs::symlink;

    use super::*;
    use crate::testing::{self, TempDir};

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn user(name: &str) -> UserName {
        UserName::parse(name.to_owned()).unwrap()
    }

    fn session(temp: &TempDir) -> StdSession {
        StdSession::at(AbsDir::normalising(temp.path().to_str().unwrap()).unwrap())
    }

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn the_default_marker_dir_is_systemds() {
        assert_eq!(
            StdSession::default().marker(&user("alice")).get(),
            "/var/lib/systemd/linger/alice"
        );
    }

    #[test]
    fn linger_is_on_for_the_users_with_a_marker_file_only() {
        let temp = TempDir::new("session-linger").unwrap();
        fs::write(temp.path().join("alice"), b"").unwrap();
        fs::create_dir(temp.path().join("dir")).unwrap();
        fs::write(temp.path().join("target"), b"").unwrap();
        symlink(temp.path().join("target"), temp.path().join("linked")).unwrap();
        symlink("/nonexistent/x", temp.path().join("dangling")).unwrap();
        let session = session(&temp);
        let seen = |name: &str| runtime().block_on(session.linger(&user(name)));
        assert_eq!(seen("alice"), Ok(Linger::On));
        assert_eq!(seen("linked"), Ok(Linger::On));
        assert_eq!(seen("bob"), Ok(Linger::Off));
        assert_eq!(seen("dir"), Ok(Linger::Off));
        assert_eq!(seen("dangling"), Ok(Linger::Off));
    }

    #[test]
    fn a_missing_linger_dir_means_off() {
        let session = StdSession::at(AbsDir::parse("/nonexistent/linger".to_owned()).unwrap());
        assert_eq!(
            runtime().block_on(session.linger(&user("alice"))),
            Ok(Linger::Off)
        );
    }

    #[test]
    fn a_program_is_judged_by_its_exit_status() {
        let _quiet = testing::one_spawn_at_a_time();
        let run = |program: &str, items: &[&str]| runtime().block_on(run(program, &args(items)));
        assert_eq!(run("true", &[]), Ok(()));
        assert_eq!(
            run("sh", &["-c", "exit 3"]),
            Err(SessionError::Failed { code: Some(3) })
        );
        assert_eq!(
            run("sh", &["-c", "kill -9 $$"]),
            Err(SessionError::Failed { code: None })
        );
        assert_eq!(
            run("amoeboid-no-such-program", &[]),
            Err(SessionError::Spawn {
                cause: Cause {
                    kind: io::ErrorKind::NotFound,
                    errno: Some(2)
                }
            })
        );
    }

    #[test]
    fn the_program_gets_no_terminal_input() {
        let _quiet = testing::one_spawn_at_a_time();
        // `cat` returns at once only when stdin is closed or empty.
        assert_eq!(runtime().block_on(run("cat", &[])), Ok(()));
    }

    #[test]
    fn the_marker_is_named_after_the_user_inside_the_dir() {
        testing::table(
            &[
                ("alice", "/l/alice".to_owned()),
                ("_x-1", "/l/_x-1".to_owned()),
            ],
            |name| {
                StdSession::at(AbsDir::parse("/l".to_owned()).unwrap())
                    .marker(&user(name))
                    .get()
                    .clone()
            },
        );
    }
}
