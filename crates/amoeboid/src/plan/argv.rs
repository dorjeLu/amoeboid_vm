//! Command lines: `nix build`, virtiofsd, ssh. An [`Argv`] has no public constructor; the
//! functions here are the only way to one, and they take refined values only.

use crate::domain::{
    AbsPath, Executable, FlakeDir, FlakePath, Gid, LoopbackPort, Mode, Uid, UserName, VmName,
};
use crate::layout::RuntimeDir;
use crate::plan::known_hosts::HostAlias;
use crate::plan::share::Share;

/// A program and its arguments.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Argv {
    program: String,
    args: Vec<String>,
}

impl Argv {
    fn new(program: &str, args: Vec<String>) -> Self {
        Self {
            program: program.to_owned(),
            args,
        }
    }

    /// Looked up on `PATH` unless absolute.
    #[must_use]
    pub fn program(&self) -> &str {
        &self.program
    }

    #[must_use]
    pub fn args(&self) -> &[String] {
        &self.args
    }

    /// Program then arguments.
    #[must_use]
    pub fn to_vec(&self) -> Vec<String> {
        std::iter::once(self.program.clone())
            .chain(self.args.iter().cloned())
            .collect()
    }
}

/// Arguments the operator gave for ssh (a remote command, or ssh options of theirs). Opaque: the
/// only use is [`ssh_argv`], which appends them after the destination, where ssh reads them as the
/// remote command.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct SshPassthrough(Vec<String>);

impl SshPassthrough {
    #[must_use]
    pub fn new(args: Vec<String>) -> Self {
        Self(args)
    }
}

/// `nix build` of the VM's own config with the `host` input overridden by its `host.nix`. Both go
/// into `path:` URLs, so they must be flake paths (no `?`, `#` or `%`, which Nix would read as
/// query, fragment and escape).
#[must_use]
pub fn nix_build_argv(flake: &FlakeDir, host_module: &FlakePath, out_link: &AbsPath) -> Argv {
    Argv::new(
        "nix",
        vec![
            "--extra-experimental-features".to_owned(),
            "nix-command flakes".to_owned(),
            "build".to_owned(),
            format!("path:{flake}"),
            "--override-input".to_owned(),
            "host".to_owned(),
            format!("path:{host_module}"),
            "--no-write-lock-file".to_owned(),
            "--out-link".to_owned(),
            out_link.get().clone(),
        ],
    )
}

/// The guest user's ids and the host user's they map onto.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IdMap {
    pub guest_uid: Uid,
    pub guest_gid: Gid,
    pub host_uid: Uid,
    pub host_gid: Gid,
}

/// virtiofsd for `share`, as the host user with the guest user's ids mapped onto it: files the
/// guest creates (even as root) are owned by the host user, and the host user's files appear in
/// the guest as the guest user's. Read-only is enforced here, by `--readonly`.
#[must_use]
pub fn virtiofsd_argv(
    binary: &Executable,
    share: &Share,
    runtime: &RuntimeDir,
    ids: IdMap,
) -> Argv {
    let mut args = vec![
        format!("--socket-path={}", runtime.socket(share.role.socket())),
        format!("--shared-dir={}", share.source),
        "--sandbox=namespace".to_owned(),
        format!("--uid-map=:{}:{}:1:", ids.guest_uid, ids.host_uid),
        format!("--gid-map=:{}:{}:1:", ids.guest_gid, ids.host_gid),
        "--log-level=error".to_owned(),
    ];
    match share.mode {
        Mode::Ro => args.push("--readonly".to_owned()),
        Mode::Rw => {}
    }
    Argv::new(binary.path().get(), args)
}

/// The microvm runner, which reads its extra qemu arguments from the working directory.
#[must_use]
pub fn runner_argv(runner: &Executable) -> Argv {
    Argv::new(runner.path().get(), Vec::new())
}

/// Turns on linger for `user`, so the user's systemd session (and the runtime dir with the VM's
/// sockets) survives logout. A user name never starts with `-`, so it can't read as an option.
#[must_use]
pub fn loginctl_argv(user: &UserName) -> Argv {
    Argv::new(
        "loginctl",
        vec!["enable-linger".to_owned(), user.to_string()],
    )
}

/// Generates the VM's host key at `file` (and `file.pub`): ed25519, no passphrase, commented with
/// the alias. `file` is absolute, so it can't read as an option.
#[must_use]
pub fn ssh_keygen_argv(alias: &HostAlias, file: &AbsPath) -> Argv {
    let args = [
        "-q",
        "-t",
        "ed25519",
        "-N",
        "",
        "-C",
        alias.as_str(),
        "-f",
        file.get(),
    ];
    Argv::new("ssh-keygen", args.map(str::to_owned).to_vec())
}

/// ssh to the guest user on the forwarded port, pinned to the VM's host key (`amoeboid-NAME` in
/// its own `known_hosts`), with every forwarding off whatever `~/.ssh/config` says: nothing of the
/// host's reaches the VM through ssh. `command` comes last, after the destination.
#[must_use]
pub fn ssh_argv(
    user: &UserName,
    port: LoopbackPort,
    vm: &VmName,
    known_hosts: &AbsPath,
    command: &SshPassthrough,
) -> Argv {
    let options = [
        format!("HostKeyAlias={}", HostAlias::of(vm)),
        format!("UserKnownHostsFile={known_hosts}"),
        "StrictHostKeyChecking=yes".to_owned(),
        "LogLevel=ERROR".to_owned(),
        "ForwardAgent=no".to_owned(),
        "ForwardX11=no".to_owned(),
        "GSSAPIDelegateCredentials=no".to_owned(),
        "PermitLocalCommand=no".to_owned(),
    ];
    let args = ["-p".to_owned(), port.to_string()]
        .into_iter()
        .chain(
            options
                .into_iter()
                .flat_map(|option| ["-o".to_owned(), option]),
        )
        .chain([format!("{user}@127.0.0.1")])
        .chain(command.0.iter().cloned())
        .collect();
    Argv::new("ssh", args)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use crate::testing;
    use serde_json::Value;

    use super::*;
    use crate::domain::Observation;
    use crate::layout::{Env, Paths, Var};
    use crate::plan::fixtures::{abs, shares_from_golden};
    use crate::plan::share::share_list;

    fn text<'a>(value: &'a Value, key: &str) -> &'a str {
        value.get(key).and_then(Value::as_str).unwrap()
    }

    fn number(value: &Value, key: &str) -> u64 {
        value.get(key).and_then(Value::as_u64).unwrap()
    }

    fn strings(value: &Value) -> Vec<String> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item.as_str().unwrap().to_owned())
            .collect()
    }

    fn golden(name: &str) -> Vec<Value> {
        let text = match name {
            "nix_build_argv" => include_str!("../../tests/golden/nix_build_argv.json"),
            "ssh_argv" => include_str!("../../tests/golden/ssh_argv.json"),
            _ => include_str!("../../tests/golden/shares.json"),
        };
        let cases: Vec<Value> = serde_json::from_str(text).unwrap();
        assert!(cases.len() > 1);
        cases
    }

    fn path(text: &str) -> AbsPath {
        AbsPath::parse(text.to_owned()).unwrap()
    }

    #[test]
    fn nix_build_argv_matches_the_python_one_for_every_path_nix_accepts() {
        let mut compared = 0;
        let mut refused = 0;
        for case in golden("nix_build_argv") {
            let input = case.get("input").unwrap();
            let (flake, module) = (text(input, "flake"), text(input, "host_module"));
            // Python passed any path on; the Rust side refuses those Nix would misread or reject
            // (design, Compatibility), so these cases only show the refusal.
            let (Ok(flake_dir), Ok(module)) =
                (FlakeDir::parse(abs(flake)), FlakePath::parse(path(module)))
            else {
                refused += 1;
                continue;
            };
            compared += 1;
            let argv = nix_build_argv(&flake_dir, &module, &path(text(input, "out_link")));
            assert_eq!(argv.program(), "nix");
            assert_eq!(
                argv.to_vec(),
                strings(case.get("output").unwrap()),
                "{case}"
            );
        }
        // The cases with a space and with non-ASCII characters.
        assert_eq!((compared, refused), (2, 2));
    }

    #[test]
    fn ssh_argv_matches_the_python_one() {
        for case in golden("ssh_argv") {
            let input = case.get("input").unwrap();
            let vm = text(input, "alias").strip_prefix("amoeboid-").unwrap();
            let argv = ssh_argv(
                &UserName::parse(text(input, "user").to_owned()).unwrap(),
                LoopbackPort::parse(u16::try_from(number(input, "port")).unwrap()).unwrap(),
                &VmName::parse(vm.to_owned()).unwrap(),
                &path(text(input, "known_hosts")),
                &SshPassthrough::new(strings(input.get("command").unwrap())),
            );
            assert_eq!(
                argv.to_vec(),
                strings(case.get("output").unwrap()),
                "{case}"
            );
        }
    }

    #[test]
    fn loginctl_enables_linger_for_exactly_the_user() {
        testing::table(
            &[
                (
                    "alice",
                    ["loginctl", "enable-linger", "alice"]
                        .map(str::to_owned)
                        .to_vec(),
                ),
                (
                    "_svc-1",
                    ["loginctl", "enable-linger", "_svc-1"]
                        .map(str::to_owned)
                        .to_vec(),
                ),
            ],
            |name| loginctl_argv(&UserName::parse((*name).to_owned()).unwrap()).to_vec(),
        );
    }

    #[test]
    fn keygen_makes_a_passphraseless_ed25519_key_commented_with_the_alias() {
        let argv = ssh_keygen_argv(
            &HostAlias::of(&VmName::parse("proj".to_owned()).unwrap()),
            &path("/d/proj/ssh/.keygen/ssh_host_ed25519_key"),
        );
        assert_eq!(
            argv.to_vec(),
            [
                "ssh-keygen",
                "-q",
                "-t",
                "ed25519",
                "-N",
                "",
                "-C",
                "amoeboid-proj",
                "-f",
                "/d/proj/ssh/.keygen/ssh_host_ed25519_key"
            ]
            .map(str::to_owned)
            .to_vec()
        );
    }

    #[test]
    fn passthrough_arguments_come_only_after_the_destination() {
        let argv = ssh_argv(
            &UserName::parse("u".to_owned()).unwrap(),
            LoopbackPort::parse(2222).unwrap(),
            &VmName::parse("v".to_owned()).unwrap(),
            &path("/k"),
            &SshPassthrough::new(vec!["-o".to_owned(), "ProxyCommand=x".to_owned()]),
        );
        let destination = argv.args().iter().position(|arg| arg == "u@127.0.0.1");
        assert_eq!(destination, Some(18));
        assert_eq!(
            argv.args().get(19..),
            Some(["-o".to_owned(), "ProxyCommand=x".to_owned()].as_slice())
        );
        let empty = ssh_argv(
            &UserName::parse("u".to_owned()).unwrap(),
            LoopbackPort::parse(2222).unwrap(),
            &VmName::parse("v".to_owned()).unwrap(),
            &path("/k"),
            &SshPassthrough::default(),
        );
        assert_eq!(empty.args().last().map(String::as_str), Some("u@127.0.0.1"));
    }

    /// The runtime dir of VM `name` under `root`.
    fn runtime(root: &str, name: &str) -> RuntimeDir {
        let env = Env {
            home: Var::Text("/h".to_owned()),
            xdg_runtime_dir: Var::Text(root.to_owned()),
            ..Env::UNSET
        };
        let paths = Paths::resolve(&env, Uid::parse(1).unwrap()).unwrap();
        paths
            .vm(&VmName::parse(name.to_owned()).unwrap())
            .runtime()
            .clone()
    }

    fn executable(text: &str) -> Executable {
        let file = path(text);
        Executable::observe(&file, |_| Observation::File {
            resolved: path(text),
            executable: true,
        })
        .unwrap()
    }

    #[test]
    fn the_runner_is_run_by_its_path_alone() {
        assert_eq!(
            runner_argv(&executable("/nix/store/x-runner/bin/microvm-run")).to_vec(),
            ["/nix/store/x-runner/bin/microvm-run"]
        );
    }

    #[test]
    fn virtiofsd_argv_matches_the_python_one_for_every_share() {
        for case in golden("shares") {
            let input = case.get("input").unwrap();
            let (root, name) = text(input, "runtime").rsplit_once("/amb_").unwrap();
            let id = |key| u32::try_from(number(input, key)).unwrap();
            let ids = IdMap {
                guest_uid: Uid::parse(id("guest_uid")).unwrap(),
                guest_gid: Gid::parse(id("guest_gid")).unwrap(),
                host_uid: Uid::parse(id("host_uid")).unwrap(),
                host_gid: Gid::parse(id("host_gid")).unwrap(),
            };
            let mounts = shares_from_golden(input.get("mounts").unwrap());
            let list = share_list(&abs(text(input, "guest_state")), &mounts);
            let got: Vec<Vec<String>> = list
                .iter()
                .map(|share| {
                    virtiofsd_argv(
                        &executable(text(input, "binary")),
                        share,
                        &runtime(root, name),
                        ids,
                    )
                    .to_vec()
                })
                .collect();
            let expected: Vec<Vec<String>> = case
                .get("output")
                .and_then(|o| o.get("virtiofsd_argv"))
                .and_then(Value::as_array)
                .unwrap()
                .iter()
                .map(strings)
                .collect();
            assert_eq!(got, expected, "{}", text(&case, "name"));
        }
    }
}
