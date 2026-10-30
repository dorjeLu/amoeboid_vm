//! `StdSsh`: [`Ssh`] on `ssh-keygen` from `PATH`, std files and tokio sockets.

use std::fs;
use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr};
use std::os::unix::fs::DirBuilderExt as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use tokio::io::AsyncReadExt as _;
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

use crate::domain::{AbsDir, AbsPath, HostPublicKey, LoopbackPort, SshBanner};
use crate::layout::VmPaths;
use crate::plan::{HostAlias, ssh_keygen_argv};
use crate::ports::Cause;
use crate::ports::host_fs::FsOp;
use crate::ports::ssh::{BANNER_WAIT, BannerError, BannerPhase, FreePortError, KeyError, Ssh};

/// The most an identification line may be, line end included (RFC 4253).
const BANNER_BYTES: usize = 255;

/// The real thing; `banner_wait` bounds each wait of the banner probe.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StdSsh {
    banner_wait: Duration,
}

impl StdSsh {
    #[must_use]
    pub const fn new(banner_wait: Duration) -> Self {
        Self { banner_wait }
    }
}

impl Default for StdSsh {
    fn default() -> Self {
        Self::new(BANNER_WAIT)
    }
}

impl Ssh for StdSsh {
    fn ensure_host_key(
        &self,
        vm: &VmPaths,
    ) -> impl Future<Output = Result<HostPublicKey, KeyError>> + Send {
        let vm = vm.clone();
        async move {
            let argv = ssh_keygen_argv(&HostAlias::of(vm.name()), &vm.staged_host_key());
            tokio::task::spawn_blocking(move || ensure_key(&vm, argv.program(), argv.args()))
                .await
                .unwrap_or(Err(KeyError::Aborted))
        }
    }

    async fn free_port(&self) -> Result<LoopbackPort, FreePortError> {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .map_err(|error| bind_error(&error))?;
        let address = listener.local_addr().map_err(|error| bind_error(&error))?;
        refine_port(address.port())
    }

    fn banner(
        &self,
        port: LoopbackPort,
    ) -> impl Future<Output = Result<SshBanner, BannerError>> + Send {
        let wait = self.banner_wait;
        async move { banner(port, wait).await }
    }

    async fn accepts(&self, port: LoopbackPort) -> bool {
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, *port.get()));
        matches!(
            timeout(CONNECT_WAIT, TcpStream::connect(address)).await,
            Ok(Ok(_))
        )
    }

    async fn port_free(&self, port: LoopbackPort) -> bool {
        TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, *port.get())))
            .await
            .is_ok()
    }
}

/// How long a connect to the loopback may take before it counts as refused.
const CONNECT_WAIT: Duration = Duration::from_secs(1);

fn bind_error(error: &std::io::Error) -> FreePortError {
    FreePortError::Bind {
        cause: Cause::of(error),
    }
}

/// The kernel's answer as a port.
fn refine_port(number: u16) -> Result<LoopbackPort, FreePortError> {
    LoopbackPort::parse(number).map_err(FreePortError::Unusable)
}

async fn banner(port: LoopbackPort, wait: Duration) -> Result<SshBanner, BannerError> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, *port.get()));
    let mut stream = timeout(wait, TcpStream::connect(address))
        .await
        .map_err(|_| BannerError::Timeout {
            port,
            phase: BannerPhase::Connect,
        })?
        .map_err(|error| BannerError::Connect {
            port,
            cause: Cause::of(&error),
        })?;
    let mut line = Vec::new();
    let read = timeout(wait, read_line(&mut stream, &mut line)).await;
    match read {
        Ok(Err(error)) => {
            return Err(BannerError::Read {
                port,
                cause: Cause::of(&error),
            });
        }
        // Silent: nothing came before the deadline. Some bytes did: judge those, as the first
        // few bytes are all that tells sshd from anything else.
        Err(_) if line.is_empty() => {
            return Err(BannerError::Timeout {
                port,
                phase: BannerPhase::Banner,
            });
        }
        Ok(Ok(())) if line.is_empty() => return Err(BannerError::Closed { port }),
        Ok(Ok(())) | Err(_) => {}
    }
    let first = line.split(|&byte| byte == b'\n').next().unwrap_or_default();
    let first = first.strip_suffix(b"\r").unwrap_or(first);
    SshBanner::parse(String::from_utf8_lossy(first).into_owned())
        .map_err(|error| BannerError::Invalid { port, error })
}

/// Reads into `line` until it holds a newline, [`BANNER_BYTES`] bytes, or the peer closes.
async fn read_line(stream: &mut TcpStream, line: &mut Vec<u8>) -> std::io::Result<()> {
    let mut chunk = [0_u8; BANNER_BYTES];
    while !line.contains(&b'\n') && line.len() < BANNER_BYTES {
        let count = stream.read(&mut chunk).await?;
        if count == 0 {
            break;
        }
        line.extend(chunk.iter().take(count));
    }
    line.truncate(BANNER_BYTES);
    Ok(())
}

// -- host key -------------------------------------------------------------------------

fn raw(path: &AbsPath) -> &Path {
    Path::new(path.get())
}

fn raw_dir(path: &AbsDir) -> &Path {
    Path::new(path.get())
}

fn io_error(op: FsOp, path: &AbsPath, error: &std::io::Error) -> KeyError {
    KeyError::Io {
        op,
        path: path.clone(),
        cause: Cause::of(error),
    }
}

fn dir_error(op: FsOp, dir: &AbsDir, error: &std::io::Error) -> KeyError {
    KeyError::Io {
        op,
        path: dir.to_path(),
        cause: Cause::of(error),
    }
}

/// Removes the staging directory when it goes out of scope, whether the key was committed or not.
struct Staging<'a>(&'a Path);

impl Drop for Staging<'_> {
    fn drop(&mut self) {
        // Best effort: a leftover is cleared at the next attempt and never used.
        let _ = fs::remove_dir_all(self.0);
    }
}

/// The VM's public host key, after generating the pair if the private key isn't there. `program`
/// and `args` are the `ssh-keygen` command line (the program is a parameter for tests).
fn ensure_key(vm: &VmPaths, program: &str, args: &[String]) -> Result<HostPublicKey, KeyError> {
    let key = vm.host_key();
    let present = match fs::symlink_metadata(raw(&key)) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(io_error(FsOp::Stat, &key, &error)),
    };
    if present {
        return read_public(&vm.host_key_pub());
    }
    let ssh_dir = vm.ssh_dir();
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(raw_dir(&ssh_dir))
        .map_err(|error| dir_error(FsOp::Create, &ssh_dir, &error))?;
    let staging = vm.host_key_staging();
    clear(&staging)?;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(raw_dir(&staging))
        .map_err(|error| dir_error(FsOp::Create, &staging, &error))?;
    let _cleanup = Staging(raw_dir(&staging));
    generate(program, args)?;
    let (staged_key, staged_pub) = (vm.staged_host_key(), vm.staged_host_key_pub());
    // Judge the new key before it is committed: a pair that can't be used never gets its name.
    let public = read_public(&staged_pub)?;
    for file in [&staged_pub, &staged_key] {
        sync(file)?;
    }
    // The public half first: the private key's presence marks a complete pair.
    for (from, to) in [(&staged_pub, vm.host_key_pub()), (&staged_key, key)] {
        fs::rename(raw(from), raw(&to)).map_err(|error| io_error(FsOp::Rename, &to, &error))?;
    }
    fs::File::open(raw_dir(&ssh_dir))
        .and_then(|dir| dir.sync_all())
        .map_err(|error| dir_error(FsOp::Sync, &ssh_dir, &error))?;
    Ok(public)
}

/// Removes whatever is at `dir`, a leftover of an interrupted attempt.
fn clear(dir: &AbsDir) -> Result<(), KeyError> {
    let path = raw_dir(dir);
    let removed = match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) => Err(error),
    };
    match removed {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(dir_error(FsOp::Remove, dir, &error))
        }
        Err(_) | Ok(()) => Ok(()),
    }
}

fn generate(program: &str, args: &[String]) -> Result<(), KeyError> {
    let status = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .map_err(|error| KeyError::Spawn {
            cause: Cause::of(&error),
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(KeyError::Failed {
            code: status.code(),
        })
    }
}

fn sync(file: &AbsPath) -> Result<(), KeyError> {
    fs::File::open(raw(file))
        .and_then(|open| open.sync_all())
        .map_err(|error| io_error(FsOp::Sync, file, &error))
}

fn read_public(file: &AbsPath) -> Result<HostPublicKey, KeyError> {
    let bytes = fs::read(raw(file)).map_err(|error| io_error(FsOp::Read, file, &error))?;
    HostPublicKey::parse(&String::from_utf8_lossy(&bytes)).map_err(|error| KeyError::Key {
        path: file.clone(),
        error,
    })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::io;
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    use std::time::Duration;

    use tokio::io::AsyncWriteExt as _;

    use super::*;
    use crate::domain::{SshBannerError, Uid, VmName};
    use crate::layout::{Env, Paths, Var};
    use crate::testing::{self, TempDir, one_spawn_at_a_time};

    const QUICK: Duration = Duration::from_millis(300);

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
        (temp, vm)
    }

    /// The real `ssh-keygen` command line for `vm`.
    fn keygen(vm: &VmPaths) -> (String, Vec<String>) {
        let argv = ssh_keygen_argv(&HostAlias::of(vm.name()), &vm.staged_host_key());
        (argv.program().to_owned(), argv.args().to_vec())
    }

    fn ensure(vm: &VmPaths) -> Result<HostPublicKey, KeyError> {
        let (program, args) = keygen(vm);
        ensure_key(vm, &program, &args)
    }

    fn exists(path: &AbsPath) -> bool {
        fs::symlink_metadata(path.get()).is_ok()
    }

    fn mode(path: &AbsPath) -> u32 {
        fs::metadata(path.get()).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn a_new_vm_gets_a_key_pair_renamed_into_place_and_no_staging_directory_is_left() {
        let _spawning = one_spawn_at_a_time();
        let (_temp, vm) = setup("keygen-new");
        let key = ensure(&vm).unwrap();
        assert_eq!(
            HostPublicKey::parse(&fs::read_to_string(vm.host_key_pub().get()).unwrap()),
            Ok(key)
        );
        assert_eq!(mode(&vm.host_key()), 0o600);
        assert_eq!(mode(&vm.ssh_dir().to_path()), 0o700);
        assert!(
            fs::read_to_string(vm.host_key_pub().get())
                .unwrap()
                .trim_end()
                .ends_with(" amoeboid-proj")
        );
        assert!(!exists(&vm.host_key_staging().to_path()));
    }

    #[test]
    fn an_existing_key_is_kept_and_its_public_half_returned() {
        let _spawning = one_spawn_at_a_time();
        let (_temp, vm) = setup("keygen-again");
        let first = ensure(&vm).unwrap();
        let private = fs::read(vm.host_key().get()).unwrap();
        assert_eq!(ensure(&vm), Ok(first));
        assert_eq!(fs::read(vm.host_key().get()).unwrap(), private);
    }

    #[test]
    fn a_half_generated_staging_dir_is_ignored_and_cleared() {
        let _spawning = one_spawn_at_a_time();
        let (_temp, vm) = setup("keygen-half");
        let staging = vm.host_key_staging();
        fs::create_dir_all(staging.get()).unwrap();
        fs::write(vm.staged_host_key().get(), "half a private key").unwrap();
        fs::write(vm.staged_host_key_pub().get(), "ssh-ed25519 AAAA").unwrap();
        fs::write(format!("{}/other", staging.get()), "x").unwrap();
        let key = ensure(&vm).unwrap();
        assert_ne!(
            fs::read(vm.host_key().get()).unwrap(),
            b"half a private key".to_vec()
        );
        assert_eq!(
            HostPublicKey::parse(&fs::read_to_string(vm.host_key_pub().get()).unwrap()),
            Ok(key)
        );
        assert!(!exists(&staging.to_path()));
    }

    #[test]
    fn a_staging_path_that_is_a_file_or_a_symlink_is_cleared_too() {
        let _spawning = one_spawn_at_a_time();
        for (label, plant) in [("file", true), ("link", false)] {
            let (temp, vm) = setup(&format!("keygen-odd-{label}"));
            fs::create_dir_all(vm.ssh_dir().get()).unwrap();
            let staging = vm.host_key_staging();
            if plant {
                fs::write(staging.get(), "x").unwrap();
            } else {
                let elsewhere = temp.path().join("elsewhere");
                fs::create_dir(&elsewhere).unwrap();
                fs::write(elsewhere.join("keep"), "x").unwrap();
                symlink(&elsewhere, staging.get()).unwrap();
            }
            assert!(ensure(&vm).is_ok(), "{label}");
            assert!(!exists(&staging.to_path()), "{label}");
            if !plant {
                assert_eq!(
                    fs::read_to_string(temp.path().join("elsewhere/keep")).unwrap(),
                    "x"
                );
            }
        }
    }

    #[test]
    fn a_public_half_without_the_private_key_is_replaced_by_a_new_pair() {
        let _spawning = one_spawn_at_a_time();
        let (_temp, vm) = setup("keygen-halfcommit");
        fs::create_dir_all(vm.ssh_dir().get()).unwrap();
        fs::write(vm.host_key_pub().get(), "old public half").unwrap();
        let key = ensure(&vm).unwrap();
        assert_eq!(
            HostPublicKey::parse(&fs::read_to_string(vm.host_key_pub().get()).unwrap()),
            Ok(key)
        );
        assert!(exists(&vm.host_key()));
    }

    #[test]
    fn an_existing_private_key_with_an_unusable_public_half_is_refused_naming_the_file() {
        let (_temp, vm) = setup("keygen-badpub");
        fs::create_dir_all(vm.ssh_dir().get()).unwrap();
        fs::write(vm.host_key().get(), "private").unwrap();
        fs::write(vm.host_key_pub().get(), "ssh-rsa AAAAB3Nza= me\n").unwrap();
        assert_eq!(
            ensure(&vm),
            Err(KeyError::Key {
                path: vm.host_key_pub(),
                error: crate::domain::HostPublicKeyError::Type(
                    crate::domain::KeyTypeError::Unsupported {
                        found: "ssh-rsa".to_owned()
                    }
                ),
            })
        );
    }

    #[test]
    fn an_existing_private_key_without_a_public_half_is_a_read_error() {
        let (_temp, vm) = setup("keygen-nopub");
        fs::create_dir_all(vm.ssh_dir().get()).unwrap();
        fs::write(vm.host_key().get(), "private").unwrap();
        assert_eq!(
            ensure(&vm),
            Err(KeyError::Io {
                op: FsOp::Read,
                path: vm.host_key_pub(),
                cause: Cause {
                    kind: io::ErrorKind::NotFound,
                    errno: Some(2)
                },
            })
        );
    }

    /// Runs `script` as the key generator; `$1` is the staged private key's path.
    fn scripted(vm: &VmPaths, script: &str) -> Result<HostPublicKey, KeyError> {
        let args = vec![
            "-c".to_owned(),
            script.to_owned(),
            "sh".to_owned(),
            vm.staged_host_key().get().clone(),
        ];
        ensure_key(vm, "sh", &args)
    }

    #[test]
    fn a_generator_that_fails_leaves_no_key_and_no_staging_directory() {
        let _spawning = one_spawn_at_a_time();
        let cases = [
            ("exit 3", Err(KeyError::Failed { code: Some(3) })),
            ("kill -KILL $$", Err(KeyError::Failed { code: None })),
        ];
        for (index, (script, expected)) in cases.into_iter().enumerate() {
            let (_temp, vm) = setup(&format!("keygen-fail-{index}"));
            assert_eq!(scripted(&vm, script), expected, "case {index}");
            assert!(!exists(&vm.host_key()), "case {index}");
            assert!(!exists(&vm.host_key_pub()), "case {index}");
            assert!(!exists(&vm.host_key_staging().to_path()), "case {index}");
        }
    }

    #[test]
    fn a_generator_that_is_not_there_is_a_spawn_failure() {
        let _spawning = one_spawn_at_a_time();
        let (_temp, vm) = setup("keygen-missing");
        assert_eq!(
            ensure_key(&vm, "/nonexistent/ssh-keygen", &[]),
            Err(KeyError::Spawn {
                cause: Cause {
                    kind: io::ErrorKind::NotFound,
                    errno: Some(2)
                },
            })
        );
    }

    #[test]
    fn a_generator_that_writes_nothing_is_a_read_error_on_the_staged_file() {
        let _spawning = one_spawn_at_a_time();
        let (_temp, vm) = setup("keygen-nothing");
        assert_eq!(
            scripted(&vm, "exit 0"),
            Err(KeyError::Io {
                op: FsOp::Read,
                path: vm.staged_host_key_pub(),
                cause: Cause {
                    kind: io::ErrorKind::NotFound,
                    errno: Some(2)
                },
            })
        );
        assert!(!exists(&vm.host_key()));
    }

    #[test]
    fn a_key_of_another_type_is_refused_before_it_gets_its_name() {
        let _spawning = one_spawn_at_a_time();
        let (_temp, vm) = setup("keygen-rsa");
        let error = scripted(
            &vm,
            "echo 'ssh-rsa AAAAB3Nza= c' > \"$1.pub\"; echo private > \"$1\"",
        );
        assert_eq!(
            error,
            Err(KeyError::Key {
                path: vm.staged_host_key_pub(),
                error: crate::domain::HostPublicKeyError::Type(
                    crate::domain::KeyTypeError::Unsupported {
                        found: "ssh-rsa".to_owned()
                    }
                ),
            })
        );
        assert!(!exists(&vm.host_key()));
        assert!(!exists(&vm.host_key_pub()));
        assert!(!exists(&vm.host_key_staging().to_path()));
    }

    #[test]
    fn an_unwritable_ssh_dir_is_a_create_error_on_the_staging_directory() {
        if rustix::process::geteuid().is_root() {
            return;
        }
        let (_temp, vm) = setup("keygen-readonly");
        fs::create_dir_all(vm.ssh_dir().get()).unwrap();
        fs::set_permissions(vm.ssh_dir().get(), fs::Permissions::from_mode(0o500)).unwrap();
        let result = ensure(&vm);
        fs::set_permissions(vm.ssh_dir().get(), fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            result,
            Err(KeyError::Io {
                op: FsOp::Create,
                path: vm.host_key_staging().to_path(),
                cause: Cause {
                    kind: io::ErrorKind::PermissionDenied,
                    errno: Some(13)
                },
            })
        );
    }

    #[test]
    fn the_free_port_is_never_one_somebody_listens_on() {
        let rt = runtime();
        // Ports other tests bind meanwhile can't be told apart from a pick, so the check is
        // against listeners held here for the whole run.
        let held: Vec<_> = (0..16)
            .map(|_| std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap())
            .collect();
        let taken: Vec<u16> = held
            .iter()
            .map(|listener| listener.local_addr().unwrap().port())
            .collect();
        for _ in 0..16 {
            let port = rt.block_on(StdSsh::default().free_port()).unwrap();
            assert!(!taken.contains(port.get()), "{port}");
        }
    }

    #[test]
    fn a_port_number_zero_from_the_kernel_is_unusable() {
        testing::table(
            &[
                (
                    0,
                    Err(FreePortError::Unusable(
                        crate::domain::LoopbackPortError::Zero,
                    )),
                ),
                (1, LoopbackPort::parse(1).map_err(FreePortError::Unusable)),
                (
                    65535,
                    LoopbackPort::parse(65535).map_err(FreePortError::Unusable),
                ),
            ],
            |&number| refine_port(number),
        );
    }

    /// A listener on a free port that runs `serve` on its first connection.
    fn listen<F, Fut>(rt: &tokio::runtime::Runtime, serve: F) -> LoopbackPort
    where
        F: FnOnce(TcpStream) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let _guard = rt.enter();
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let listener = TcpListener::from_std(listener).unwrap();
        rt.spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                serve(stream).await;
            }
        });
        LoopbackPort::parse(port).unwrap()
    }

    fn probe(rt: &tokio::runtime::Runtime, port: LoopbackPort) -> Result<SshBanner, BannerError> {
        rt.block_on(StdSsh::new(QUICK).banner(port))
    }

    fn banner_of(text: &str) -> SshBanner {
        SshBanner::parse(text.to_owned()).unwrap()
    }

    /// Sends `bytes`, then holds the connection open (or closes it).
    fn sends(rt: &tokio::runtime::Runtime, bytes: Vec<u8>, hold: bool) -> LoopbackPort {
        listen(rt, move |mut stream| async move {
            let _ = stream.write_all(&bytes).await;
            if hold {
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        })
    }

    #[test]
    fn a_listener_accepts_and_holds_its_port_and_a_closed_port_is_free() {
        let rt = runtime();
        let held = sends(&rt, Vec::new(), true);
        assert!(rt.block_on(StdSsh::new(QUICK).accepts(held)));
        assert!(!rt.block_on(StdSsh::new(QUICK).port_free(held)));
        // Bind to learn a free number, then let it go.
        let free = {
            let _guard = rt.enter();
            let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            LoopbackPort::parse(listener.local_addr().unwrap().port()).unwrap()
        };
        assert!(!rt.block_on(StdSsh::new(QUICK).accepts(free)));
        assert!(rt.block_on(StdSsh::new(QUICK).port_free(free)));
    }

    #[test]
    fn a_sshd_banner_line_is_the_banner() {
        let rt = runtime();
        let port = sends(&rt, b"SSH-2.0-OpenSSH_9.9 Debian\r\n".to_vec(), true);
        assert_eq!(
            probe(&rt, port),
            Ok(banner_of("SSH-2.0-OpenSSH_9.9 Debian"))
        );
        let bare = sends(&rt, b"SSH-2.0-x\n".to_vec(), true);
        assert_eq!(probe(&rt, bare), Ok(banner_of("SSH-2.0-x")));
    }

    #[test]
    fn a_banner_that_arrived_in_part_counts_when_what_came_is_one() {
        let rt = runtime();
        let port = sends(&rt, b"SSH-2.0-Open".to_vec(), true);
        assert_eq!(probe(&rt, port), Ok(banner_of("SSH-2.0-Open")));
        let closed = sends(&rt, b"SSH-2.0-Open".to_vec(), false);
        assert_eq!(probe(&rt, closed), Ok(banner_of("SSH-2.0-Open")));
    }

    #[test]
    fn a_listener_that_says_nothing_times_out_waiting_for_the_banner() {
        let rt = runtime();
        let port = sends(&rt, Vec::new(), true);
        assert_eq!(
            probe(&rt, port),
            Err(BannerError::Timeout {
                port,
                phase: BannerPhase::Banner
            })
        );
    }

    #[test]
    fn a_connection_closed_without_a_byte_is_closed() {
        let rt = runtime();
        let port = sends(&rt, Vec::new(), false);
        assert_eq!(probe(&rt, port), Err(BannerError::Closed { port }));
    }

    #[test]
    fn something_other_than_sshd_is_an_invalid_banner() {
        let rt = runtime();
        let cases: [(&[u8], SshBannerError); 5] = [
            (b"HTTP/1.1 400 Bad Request\r\n", SshBannerError::NotSsh),
            (b"\r\n", SshBannerError::NotSsh),
            (b"SSH-\r\n", SshBannerError::NoVersion),
            (
                b"SSH-2.0-a\x1b[2J\r\n",
                SshBannerError::BadChar {
                    ch: '\u{1b}',
                    at: 9,
                },
            ),
            (
                b"SSH-2.0-\xff\r\n",
                SshBannerError::BadChar {
                    ch: '\u{fffd}',
                    at: 8,
                },
            ),
        ];
        for (index, (bytes, error)) in cases.into_iter().enumerate() {
            let port = sends(&rt, bytes.to_vec(), true);
            assert_eq!(
                probe(&rt, port),
                Err(BannerError::Invalid { port, error }),
                "case {index}"
            );
        }
    }

    #[test]
    fn an_endless_line_is_cut_at_255_bytes_and_refused() {
        let rt = runtime();
        let mut bytes = b"SSH-".to_vec();
        bytes.extend(std::iter::repeat_n(b'x', 400));
        let port = sends(&rt, bytes, true);
        assert_eq!(
            probe(&rt, port),
            Err(BannerError::Invalid {
                port,
                error: SshBannerError::TooLong { len: 255 }
            })
        );
    }

    #[test]
    fn a_port_nobody_listens_on_is_refused() {
        let rt = runtime();
        let port = {
            let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            LoopbackPort::parse(listener.local_addr().unwrap().port()).unwrap()
        };
        assert_eq!(
            probe(&rt, port),
            Err(BannerError::Connect {
                port,
                cause: Cause {
                    kind: io::ErrorKind::ConnectionRefused,
                    errno: Some(111)
                },
            })
        );
    }

    #[test]
    #[allow(
        deprecated,
        reason = "a zero linger is how a test makes the close a reset; tokio's warning is about blocking on drop"
    )]
    fn a_connection_reset_while_waiting_is_a_read_error() {
        let rt = runtime();
        let port = listen(&rt, |stream| async move {
            // A zero linger makes the close a reset instead of an orderly shutdown.
            let _ = stream.set_linger(Some(Duration::ZERO));
            drop(stream);
        });
        assert_eq!(
            probe(&rt, port),
            Err(BannerError::Read {
                port,
                cause: Cause {
                    kind: io::ErrorKind::ConnectionReset,
                    errno: Some(104)
                },
            })
        );
    }

    #[test]
    fn the_default_waits_as_long_as_the_port_says() {
        assert_eq!(StdSsh::default(), StdSsh::new(BANNER_WAIT));
    }
}
