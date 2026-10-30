//! The guest's ssh identity: generate the host key once per VM and pin it in the VM's
//! `known_hosts`, so the host recognises the guest by a key the guest can't swap (the key lives
//! outside the guest-writable share and is copied in on every start).

use crate::diagnostic::Diagnostic;
use crate::domain::{FileMode, HostPublicKey};
use crate::layout::VmPaths;
use crate::plan::known_hosts;
use crate::ports::host_fs::{FsError, HostFs};
use crate::ports::ssh::{KeyError, Ssh};
use crate::refined;

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum HostKeyError {
    #[error(transparent)]
    Key(KeyError),
    /// `known_hosts` could not be written.
    #[error(transparent)]
    Pin(FsError),
}

impl Diagnostic for HostKeyError {
    fn code(&self) -> &'static str {
        match self {
            Self::Key(error) => error.code(),
            Self::Pin(_) => "host-key.pin",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::Key(error) => error.hint(),
            Self::Pin(error) => error.hint(),
        }
    }
}

/// The VM's host key (made if there is none), pinned in its `known_hosts`. The pin is rewritten
/// every time, so it always names the key that is in the VM's `ssh/` directory. The caller holds
/// the VM lock.
///
/// # Errors
/// The key can't be generated or read, or `known_hosts` can't be written.
pub async fn ensure(
    ssh: &impl Ssh,
    fs: &impl HostFs,
    vm: &VmPaths,
) -> Result<HostPublicKey, HostKeyError> {
    let key = ssh.ensure_host_key(vm).await.map_err(HostKeyError::Key)?;
    fs.write_file(
        &vm.known_hosts(),
        known_hosts(vm.name(), &key).into_bytes(),
        refined!(FileMode, 0o644),
    )
    .await
    .map_err(HostKeyError::Pin)?;
    Ok(key)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(
    clippy::disallowed_types,
    reason = "tests plant files on a real filesystem"
)]
pub(crate) mod tests {
    use std::fs;
    use std::future::{Future, ready};
    use std::io;
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;
    use crate::adapters::StdFs;
    use crate::domain::{AbsPath, LoopbackPort, SshBanner, Uid, VmName};
    use crate::layout::{Env, Paths, Var};
    use crate::ports::Cause;
    use crate::ports::host_fs::FsOp;
    use crate::ports::ssh::{BannerError, FreePortError};
    use crate::testing::{self, Entry, TempDir};

    const KEY: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIGq1ZPV0QcVQgRtuRZ8nvRNQdNnwpOAiIUUwwO8M3DJq";

    /// `ensure_host_key` answers with `answer`; the rest is never asked.
    struct Fake {
        answer: Result<HostPublicKey, KeyError>,
    }

    impl Ssh for Fake {
        fn ensure_host_key(
            &self,
            _vm: &VmPaths,
        ) -> impl Future<Output = Result<HostPublicKey, KeyError>> + Send {
            ready(self.answer.clone())
        }

        fn free_port(&self) -> impl Future<Output = Result<LoopbackPort, FreePortError>> + Send {
            ready(Err(FreePortError::Bind {
                cause: Cause {
                    kind: io::ErrorKind::Other,
                    errno: None,
                },
            }))
        }

        fn banner(
            &self,
            port: LoopbackPort,
        ) -> impl Future<Output = Result<SshBanner, BannerError>> + Send {
            ready(Err(BannerError::Closed { port }))
        }

        fn accepts(&self, _port: LoopbackPort) -> impl Future<Output = bool> + Send {
            ready(false)
        }

        fn port_free(&self, _port: LoopbackPort) -> impl Future<Output = bool> + Send {
            ready(true)
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn setup(label: &str) -> (TempDir, VmPaths) {
        let temp = TempDir::new(label).unwrap();
        let env = Env {
            home: Var::Text(format!("{}/h", temp.path().to_str().unwrap())),
            ..Env::UNSET
        };
        let paths = Paths::resolve(&env, Uid::parse(1000).unwrap()).unwrap();
        let vm = paths.vm(&VmName::parse("proj".to_owned()).unwrap());
        fs::create_dir_all(vm.ssh_dir().get()).unwrap();
        (temp, vm)
    }

    fn key() -> HostPublicKey {
        HostPublicKey::parse(&format!("ssh-ed25519 {KEY} c")).unwrap()
    }

    fn failure() -> HostKeyError {
        HostKeyError::Pin(FsError::Io {
            op: FsOp::Write,
            path: AbsPath::parse("/d/proj/ssh/known_hosts".to_owned()).unwrap(),
            entry: None,
            cause: Cause {
                kind: io::ErrorKind::StorageFull,
                errno: Some(28),
            },
        })
    }

    fn witness(e: &HostKeyError) -> &'static str {
        match e {
            HostKeyError::Key(_) => "Key",
            HostKeyError::Pin(_) => "Pin",
        }
    }

    fn errors() -> Vec<HostKeyError> {
        vec![HostKeyError::Key(KeyError::Aborted), failure()]
    }

    /// One catalogue entry per variant (a wrapped error keeps the catalogue entry of its cause,
    /// except the pin).
    pub(crate) fn entries() -> Vec<Entry> {
        vec![Entry::of(&failure())]
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(&errors(), witness, &["Key", "Pin"]);
    }

    #[test]
    fn a_wrapped_key_error_keeps_its_code_message_and_hint() {
        let inner = KeyError::Failed { code: Some(1) };
        let wrapped = HostKeyError::Key(inner.clone());
        assert_eq!(wrapped.code(), inner.code());
        assert_eq!(wrapped.message(), inner.message());
        assert_eq!(wrapped.hint(), inner.hint());
    }

    #[test]
    fn a_pin_failure_is_named_and_gets_the_hint_of_the_write() {
        assert_eq!(failure().code(), "host-key.pin");
        assert_eq!(
            failure().message(),
            "can't write /d/proj/ssh/known_hosts: no storage space"
        );
        assert_eq!(
            failure().hint().as_deref(),
            Some("free disk space and try again")
        );
    }

    #[test]
    fn the_key_is_pinned_under_the_vm_alias_with_mode_0644() {
        let (_temp, vm) = setup("pin");
        let fake = Fake { answer: Ok(key()) };
        assert_eq!(runtime().block_on(ensure(&fake, &StdFs, &vm)), Ok(key()));
        let pin = vm.known_hosts();
        assert_eq!(
            fs::read_to_string(pin.get()).unwrap(),
            format!("amoeboid-proj ssh-ed25519 {KEY}\n")
        );
        let mode = fs::metadata(pin.get()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o644);
    }

    #[test]
    fn an_old_pin_is_replaced_by_the_current_key() {
        let (_temp, vm) = setup("repin");
        fs::write(
            vm.known_hosts().get(),
            "amoeboid-proj ssh-ed25519 AAAA\nother line\n",
        )
        .unwrap();
        let fake = Fake { answer: Ok(key()) };
        runtime().block_on(ensure(&fake, &StdFs, &vm)).unwrap();
        assert_eq!(
            fs::read_to_string(vm.known_hosts().get()).unwrap(),
            format!("amoeboid-proj ssh-ed25519 {KEY}\n")
        );
    }

    #[test]
    fn a_key_that_cant_be_had_pins_nothing() {
        let (_temp, vm) = setup("nokey");
        let fake = Fake {
            answer: Err(KeyError::Failed { code: Some(1) }),
        };
        assert_eq!(
            runtime().block_on(ensure(&fake, &StdFs, &vm)),
            Err(HostKeyError::Key(KeyError::Failed { code: Some(1) }))
        );
        assert!(fs::metadata(vm.known_hosts().get()).is_err());
    }

    #[test]
    fn a_pin_that_cant_be_written_is_a_pin_error_naming_the_file() {
        let (_temp, vm) = setup("nowrite");
        fs::remove_dir_all(vm.ssh_dir().get()).unwrap();
        let fake = Fake { answer: Ok(key()) };
        let error = runtime().block_on(ensure(&fake, &StdFs, &vm)).unwrap_err();
        assert_eq!(error.code(), "host-key.pin");
        assert!(
            error.message().contains("known_hosts"),
            "{}",
            error.message()
        );
    }
}
