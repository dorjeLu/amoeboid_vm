//! The contract between the Nix side and this library (design §Contract): `contract.json`, which
//! the flake publishes beside `runner` and `virtiofsd`, decoded into proofs and compared with the
//! constants of the version this library implements. It turns the coupling table of
//! `DEVELOPING.adoc` into checked data.

use std::fmt;

use serde::{Deserialize, Deserializer, de};

use crate::diagnostic::Diagnostic;
use crate::domain::{
    AbsPath, ContractVersion, ExecutableError, GuestPath, GuestPort, HomeStorage, ShareTagName,
    Socket, StateFileName,
};
use crate::refined;

/// The tag and socket of the state share, and where the guest mounts it.
#[derive(Clone, PartialEq, Eq, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StateShare {
    pub tag: ShareTagName,
    pub socket: Socket,
    pub mount_point: GuestPath,
}

/// The tag and socket of the read-only store share.
#[derive(Clone, PartialEq, Eq, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StoreShare {
    pub tag: ShareTagName,
    pub socket: Socket,
}

/// `contract.json`: every name Python and Nix must agree on. Every field is a proof, so a value
/// of this type is well formed whatever file it came from; [`check`] says whether it is the
/// contract this library implements.
#[derive(Clone, PartialEq, Eq, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Contract {
    pub version: ContractVersion,
    #[serde(deserialize_with = "home_storage")]
    pub home: HomeStorage,
    pub control_socket: Socket,
    pub state_share: StateShare,
    pub store_share: StoreShare,
    /// The file in the runtime dir the runner's extra-args script reads.
    pub extra_args_file: StateFileName,
    /// The files the guest reads from the state share, in the order the contract lists them.
    pub state_files: Vec<StateFileName>,
    pub guest_ssh_port: GuestPort,
}

fn home_storage<'de, D: Deserializer<'de>>(deserializer: D) -> Result<HomeStorage, D::Error> {
    let text = String::deserialize(deserializer)?;
    HomeStorage::parse(&text).ok_or_else(|| {
        de::Error::custom(format!(
            "unknown home storage {text:?}, expected one of {}",
            HomeStorage::NAMES.join(", ")
        ))
    })
}

/// `contract.json` that is not JSON, or not a [`Contract`]: wrong types, missing, repeated or
/// unknown fields, a version of 0, a socket or name outside its grammar.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum ContractParseError {
    /// The decoder's own words (with the field and the line and column), escaped for display.
    #[error("{message}")]
    Invalid { message: String },
}

impl Diagnostic for ContractParseError {
    fn code(&self) -> &'static str {
        match self {
            Self::Invalid { .. } => "contract.invalid",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some(
            "rebase the VM onto a reference config whose contract this amoeboid implements"
                .to_owned(),
        )
    }
}

impl Contract {
    /// # Errors
    /// The text is not a contract; the message names the offending field.
    pub fn parse(bytes: &[u8]) -> Result<Self, ContractParseError> {
        serde_json::from_slice(bytes).map_err(|error| ContractParseError::Invalid {
            // The text may echo a hostile field name; show control characters escaped.
            message: error
                .to_string()
                .chars()
                .map(|ch| {
                    if ch.is_control() {
                        ch.escape_unicode().to_string()
                    } else {
                        ch.to_string()
                    }
                })
                .collect(),
        })
    }

    /// What this contract says about `field`.
    #[must_use]
    pub fn value(&self, field: ContractField) -> ContractValue {
        match field {
            ContractField::Version => ContractValue::Version(self.version),
            ContractField::Home => ContractValue::Home(self.home),
            ContractField::ControlSocket => ContractValue::Socket(self.control_socket),
            ContractField::StateTag => ContractValue::Tag(self.state_share.tag.clone()),
            ContractField::StateSocket => ContractValue::Socket(self.state_share.socket),
            ContractField::StateMountPoint => {
                ContractValue::Path(self.state_share.mount_point.clone())
            }
            ContractField::StoreTag => ContractValue::Tag(self.store_share.tag.clone()),
            ContractField::StoreSocket => ContractValue::Socket(self.store_share.socket),
            ContractField::ExtraArgsFile => ContractValue::File(self.extra_args_file.clone()),
            ContractField::StateFiles => ContractValue::Files(self.state_files.clone()),
            ContractField::GuestSshPort => ContractValue::Port(self.guest_ssh_port),
        }
    }
}

/// The version of the contract this library implements.
pub const VERSION: ContractVersion = refined!(ContractVersion, 1);

/// The contract of [`VERSION`]: `base.nix` as of this library. It must match `base.nix` and the
/// flake's `contract` entry by hand (the test against `contract_v1.json` pins the text).
#[must_use]
pub fn expected() -> Contract {
    Contract {
        version: VERSION,
        home: HomeStorage::Share,
        control_socket: Socket::Control,
        state_share: StateShare {
            tag: refined!(ShareTagName, "amoeboid"),
            socket: Socket::State,
            mount_point: refined!(GuestPath, "/mnt/amoeboid"),
        },
        store_share: StoreShare {
            tag: refined!(ShareTagName, "ro-store"),
            socket: Socket::Store,
        },
        extra_args_file: refined!(StateFileName, "amoeboid-qemu-args"),
        state_files: vec![
            refined!(StateFileName, "mounts"),
            refined!(StateFileName, "hostname"),
            refined!(StateFileName, "term"),
            refined!(StateFileName, "ssh_host_ed25519_key"),
        ],
        guest_ssh_port: refined!(GuestPort, 22),
    }
}

/// One field of the contract, spelled as in the JSON.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ContractField {
    Version,
    Home,
    ControlSocket,
    StateTag,
    StateSocket,
    StateMountPoint,
    StoreTag,
    StoreSocket,
    ExtraArgsFile,
    StateFiles,
    GuestSshPort,
}

impl ContractField {
    /// Every field, in the order [`check`] compares them (the version first).
    pub const ALL: [Self; 11] = [
        Self::Version,
        Self::Home,
        Self::ControlSocket,
        Self::StateTag,
        Self::StateSocket,
        Self::StateMountPoint,
        Self::StoreTag,
        Self::StoreSocket,
        Self::ExtraArgsFile,
        Self::StateFiles,
        Self::GuestSshPort,
    ];

    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Version => "version",
            Self::Home => "home",
            Self::ControlSocket => "controlSocket",
            Self::StateTag => "stateShare.tag",
            Self::StateSocket => "stateShare.socket",
            Self::StateMountPoint => "stateShare.mountPoint",
            Self::StoreTag => "storeShare.tag",
            Self::StoreSocket => "storeShare.socket",
            Self::ExtraArgsFile => "extraArgsFile",
            Self::StateFiles => "stateFiles",
            Self::GuestSshPort => "guestSshPort",
        }
    }
}

impl fmt::Display for ContractField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.key())
    }
}

/// The value of one [`ContractField`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ContractValue {
    Version(ContractVersion),
    Home(HomeStorage),
    Socket(Socket),
    Tag(ShareTagName),
    Path(GuestPath),
    File(StateFileName),
    Files(Vec<StateFileName>),
    Port(GuestPort),
}

impl fmt::Display for ContractValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Version(version) => fmt::Display::fmt(version, f),
            Self::Home(home) => fmt::Display::fmt(home, f),
            Self::Socket(socket) => fmt::Display::fmt(&socket.name(), f),
            Self::Tag(text) => fmt::Display::fmt(text, f),
            Self::Path(path) => fmt::Display::fmt(path, f),
            Self::File(name) => fmt::Display::fmt(name, f),
            Self::Files(names) => {
                let texts: Vec<&str> = names.iter().map(|name| name.get().as_str()).collect();
                write!(f, "[{}]", texts.join(", "))
            }
            Self::Port(port) => fmt::Display::fmt(port, f),
        }
    }
}

/// The programs the build provides, which must be executable.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum OutputProgram {
    Runner,
    Virtiofsd,
}

impl fmt::Display for OutputProgram {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Runner => "runner",
            Self::Virtiofsd => "virtiofsd",
        })
    }
}

/// Why a build is unusable.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum BuildError {
    /// `nix` is not installed or not on `PATH`.
    #[error("nix not found")]
    NixMissing,
    /// `nix build` ran and failed (its output is above); `None` when a signal ended it.
    #[error("nix build failed{}", status_text(*.status))]
    Failed { status: Option<i32> },
    /// The build did not leave a usable program where the launcher looks.
    #[error("the build's {program} is unusable: {error}")]
    Output {
        program: OutputProgram,
        error: ExecutableError,
    },
    /// The build's `contract` entry is not there: the config predates the contract.
    #[error("{at}: the build has no contract")]
    ContractMissing { at: AbsPath },
    /// There is something at the contract's path, but not a regular file.
    #[error("{at}: the contract isn't a regular file")]
    ContractNotAFile { at: AbsPath },
    #[error("{at}: unusable contract: {error}")]
    ContractInvalid {
        at: AbsPath,
        error: ContractParseError,
    },
    /// The first field (in [`ContractField::ALL`] order) that differs from [`expected`].
    #[error("the build's contract has {field} = {found}, but this amoeboid needs {expected}")]
    ContractMismatch {
        field: ContractField,
        expected: ContractValue,
        found: ContractValue,
    },
}

fn status_text(status: Option<i32>) -> String {
    status.map_or_else(
        || " (ended by a signal)".to_owned(),
        |code| format!(" (exit status {code})"),
    )
}

impl Diagnostic for BuildError {
    fn code(&self) -> &'static str {
        match self {
            Self::NixMissing => "build.nix-missing",
            Self::Failed { .. } => "build.failed",
            Self::Output { .. } => "build.output",
            Self::ContractMissing { .. } => "build.contract-missing",
            Self::ContractNotAFile { .. } => "build.contract-not-a-file",
            Self::ContractInvalid { .. } => "build.contract-invalid",
            Self::ContractMismatch { .. } => "build.contract-mismatch",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some(
            match self {
                Self::NixMissing => "install Nix, or add it to PATH",
                Self::Failed { .. } => "fix what nix reported above and start again",
                Self::ContractMissing { .. } => {
                    "rebase the VM onto an updated preset (--base-nix-config)"
                }
                Self::Output { .. }
                | Self::ContractNotAFile { .. }
                | Self::ContractInvalid { .. } => {
                    "rebase the VM onto the reference config that came with this amoeboid"
                }
                Self::ContractMismatch { .. } => {
                    "rebase the VM onto a reference config that matches this amoeboid"
                }
            }
            .to_owned(),
        )
    }
}

/// Whether `contract` is the one this library implements.
///
/// # Errors
/// [`BuildError::ContractMismatch`] for the first differing field; a version other than
/// [`VERSION`] is reported first.
pub fn check(contract: &Contract) -> Result<(), BuildError> {
    let want = expected();
    let differs = ContractField::ALL.iter().find_map(|&field| {
        let (expected, found) = (want.value(field), contract.value(field));
        (expected != found).then_some(BuildError::ContractMismatch {
            field,
            expected,
            found,
        })
    });
    differs.map_or(Ok(()), Err)
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::testing::{self, Entry};

    const EMITTED: &str = include_str!("../tests/golden/contract_v1.json");

    fn path(text: &str) -> AbsPath {
        AbsPath::parse(text.to_owned()).unwrap()
    }

    fn name(text: &str) -> StateFileName {
        StateFileName::parse(text.to_owned()).unwrap()
    }

    fn tag(text: &str) -> ShareTagName {
        ShareTagName::parse(text.to_owned()).unwrap()
    }

    fn witness(e: &BuildError) -> &'static str {
        match e {
            BuildError::NixMissing => "NixMissing",
            BuildError::Failed { .. } => "Failed",
            BuildError::Output { .. } => "Output",
            BuildError::ContractMissing { .. } => "ContractMissing",
            BuildError::ContractNotAFile { .. } => "ContractNotAFile",
            BuildError::ContractInvalid { .. } => "ContractInvalid",
            BuildError::ContractMismatch { .. } => "ContractMismatch",
        }
    }

    fn parse_error() -> ContractParseError {
        Contract::parse(b"{}").unwrap_err()
    }

    fn errors() -> Vec<BuildError> {
        vec![
            BuildError::NixMissing,
            BuildError::Failed { status: Some(1) },
            BuildError::Output {
                program: OutputProgram::Runner,
                error: ExecutableError::NotExecutable {
                    path: path("/data/vm/runner/bin/microvm-run"),
                },
            },
            BuildError::ContractMissing {
                at: path("/data/vm/contract"),
            },
            BuildError::ContractNotAFile {
                at: path("/data/vm/contract"),
            },
            BuildError::ContractInvalid {
                at: path("/data/vm/contract"),
                error: parse_error(),
            },
            BuildError::ContractMismatch {
                field: ContractField::Version,
                expected: ContractValue::Version(VERSION),
                found: ContractValue::Version(ContractVersion::parse(2).unwrap()),
            },
        ]
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        errors()
            .iter()
            .map(Entry::of)
            .chain([Entry::of(&parse_error())])
            .collect()
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(
            &errors(),
            witness,
            &[
                "NixMissing",
                "Failed",
                "Output",
                "ContractMissing",
                "ContractNotAFile",
                "ContractInvalid",
                "ContractMismatch",
            ],
        );
    }

    #[test]
    fn the_contract_the_reference_config_emits_is_the_one_this_library_expects() {
        assert_eq!(Contract::parse(EMITTED.as_bytes()), Ok(expected()));
        assert_eq!(check(&expected()), Ok(()));
        assert_eq!(VERSION, ContractVersion::parse(1).unwrap());
    }

    #[test]
    fn every_field_is_listed_once_and_has_its_own_key() {
        let mut keys: Vec<&str> = ContractField::ALL.iter().map(|f| f.key()).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), 11);
        assert_eq!(ContractField::ALL[0], ContractField::Version);
        // Exhaustive: a new field does not compile here until it is in ALL.
        for field in ContractField::ALL {
            match field {
                ContractField::Version
                | ContractField::Home
                | ContractField::ControlSocket
                | ContractField::StateTag
                | ContractField::StateSocket
                | ContractField::StateMountPoint
                | ContractField::StoreTag
                | ContractField::StoreSocket
                | ContractField::ExtraArgsFile
                | ContractField::StateFiles
                | ContractField::GuestSshPort => {}
            }
            assert_eq!(field.to_string(), field.key());
        }
    }

    /// The expected contract with one field changed, and the field and value `check` must report.
    fn changed() -> Vec<(Contract, ContractField, ContractValue)> {
        let base = expected();
        let with = |edit: fn(&mut Contract)| {
            let mut contract = base.clone();
            edit(&mut contract);
            contract
        };
        vec![
            (
                with(|c| c.version = ContractVersion::parse(2).unwrap()),
                ContractField::Version,
                ContractValue::Version(ContractVersion::parse(2).unwrap()),
            ),
            (
                with(|c| c.home = HomeStorage::Volume),
                ContractField::Home,
                ContractValue::Home(HomeStorage::Volume),
            ),
            (
                with(|c| c.control_socket = Socket::State),
                ContractField::ControlSocket,
                ContractValue::Socket(Socket::State),
            ),
            (
                with(|c| c.state_share.tag = tag("other")),
                ContractField::StateTag,
                ContractValue::Tag(tag("other")),
            ),
            (
                with(|c| c.state_share.socket = Socket::Control),
                ContractField::StateSocket,
                ContractValue::Socket(Socket::Control),
            ),
            (
                with(|c| {
                    c.state_share.mount_point = GuestPath::parse("/mnt/x".to_owned()).unwrap();
                }),
                ContractField::StateMountPoint,
                ContractValue::Path(GuestPath::parse("/mnt/x".to_owned()).unwrap()),
            ),
            (
                with(|c| c.store_share.tag = tag("store")),
                ContractField::StoreTag,
                ContractValue::Tag(tag("store")),
            ),
            (
                with(|c| c.store_share.socket = Socket::State),
                ContractField::StoreSocket,
                ContractValue::Socket(Socket::State),
            ),
            (
                with(|c| c.extra_args_file = name("args")),
                ContractField::ExtraArgsFile,
                ContractValue::File(name("args")),
            ),
            (
                with(|c| c.state_files.truncate(3)),
                ContractField::StateFiles,
                ContractValue::Files(vec![name("mounts"), name("hostname"), name("term")]),
            ),
            (
                with(|c| c.guest_ssh_port = GuestPort::parse(2222).unwrap()),
                ContractField::GuestSshPort,
                ContractValue::Port(GuestPort::parse(2222).unwrap()),
            ),
        ]
    }

    #[test]
    fn a_contract_differing_in_one_field_is_a_mismatch_naming_that_field() {
        let cases = changed();
        let fields: Vec<ContractField> = cases.iter().map(|(_, field, _)| *field).collect();
        assert_eq!(fields, ContractField::ALL);
        for (contract, field, found) in cases {
            assert_eq!(
                check(&contract),
                Err(BuildError::ContractMismatch {
                    field,
                    expected: expected().value(field),
                    found,
                }),
                "{field}"
            );
        }
    }

    #[test]
    fn state_files_differing_in_order_or_content_are_a_mismatch() {
        let mut reordered = expected();
        reordered.state_files.reverse();
        let mut grown = expected();
        grown.state_files.push(name("extra"));
        for contract in [reordered, grown] {
            assert!(matches!(
                check(&contract),
                Err(BuildError::ContractMismatch {
                    field: ContractField::StateFiles,
                    ..
                })
            ));
        }
    }

    #[test]
    fn with_several_differences_the_first_in_field_order_is_reported() {
        let mut contract = expected();
        contract.guest_ssh_port = GuestPort::parse(2222).unwrap();
        contract.store_share.tag = tag("store");
        contract.version = ContractVersion::parse(7).unwrap();
        let Err(BuildError::ContractMismatch { field, .. }) = check(&contract) else {
            return;
        };
        assert_eq!(field, ContractField::Version);
        contract.version = VERSION;
        let Err(BuildError::ContractMismatch { field, .. }) = check(&contract) else {
            return;
        };
        assert_eq!(field, ContractField::StoreTag);
    }

    #[test]
    fn values_and_errors_are_shown_as_the_operator_reads_the_contract() {
        testing::table(
            &[
                (ContractValue::Version(VERSION), "1".to_owned()),
                (
                    ContractValue::Home(HomeStorage::Volume),
                    "volume".to_owned(),
                ),
                (
                    ContractValue::Socket(Socket::Store),
                    "ro-store.sock".to_owned(),
                ),
                (ContractValue::Tag(tag("amoeboid")), "amoeboid".to_owned()),
                (
                    ContractValue::Path(GuestPath::parse("/mnt/amoeboid".to_owned()).unwrap()),
                    "/mnt/amoeboid".to_owned(),
                ),
                (ContractValue::File(name("mounts")), "mounts".to_owned()),
                (
                    ContractValue::Files(vec![name("mounts"), name("term")]),
                    "[mounts, term]".to_owned(),
                ),
                (ContractValue::Files(vec![]), "[]".to_owned()),
                (
                    ContractValue::Port(GuestPort::parse(22).unwrap()),
                    "22".to_owned(),
                ),
            ],
            ToString::to_string,
        );
        let [_, _, _, missing, _, _, mismatch] = <[BuildError; 7]>::try_from(errors()).unwrap();
        assert_eq!(
            mismatch.message(),
            "the build's contract has version = 2, but this amoeboid needs 1"
        );
        assert_eq!(
            missing.message(),
            "/data/vm/contract: the build has no contract"
        );
        for error in errors() {
            assert!(error.hint().is_some());
        }
        let [nix, failed, output, ..] = <[BuildError; 7]>::try_from(errors()).unwrap();
        assert_eq!(nix.message(), "nix not found");
        assert_eq!(failed.message(), "nix build failed (exit status 1)");
        assert_eq!(
            BuildError::Failed { status: None }.message(),
            "nix build failed (ended by a signal)"
        );
        assert_eq!(
            output.message(),
            "the build's runner is unusable: /data/vm/runner/bin/microvm-run: not executable"
        );
        assert_eq!(
            BuildError::ContractNotAFile {
                at: path("/data/vm/contract")
            }
            .message(),
            "/data/vm/contract: the contract isn't a regular file"
        );
    }

    /// The emitted contract with `from` replaced by `to`.
    fn edited(from: &str, to: &str) -> Vec<u8> {
        assert!(EMITTED.contains(from), "{from}");
        EMITTED.replacen(from, to, 1).into_bytes()
    }

    fn invalid(bytes: &[u8]) -> String {
        match Contract::parse(bytes) {
            Err(ContractParseError::Invalid { message }) => message,
            Ok(contract) => format!("accepted: {contract:?}"),
        }
    }

    #[test]
    #[allow(clippy::too_many_lines, reason = "one table of hostile inputs")]
    fn hostile_contracts_are_refused_with_a_message_naming_the_problem() {
        let cases: Vec<(Vec<u8>, &str)> = vec![
            (b"".to_vec(), "EOF"),
            (b"not json".to_vec(), "expected ident"),
            (b"[]".to_vec(), "expected struct Contract"),
            (b"null".to_vec(), "expected struct Contract"),
            (b"{}".to_vec(), "missing field `version`"),
            (
                edited("\"version\":1", "\"version\":0"),
                "0 is not a version",
            ),
            (edited("\"version\":1", "\"version\":-1"), "invalid value"),
            (
                edited("\"version\":1", "\"version\":\"1\""),
                "invalid type: string",
            ),
            (
                edited("\"version\":1", "\"version\":4294967296"),
                "invalid value",
            ),
            (
                edited("\"version\":1", "\"version\":1.5"),
                "invalid type: floating point",
            ),
            (edited(",\"version\":1}", "}"), "missing field `version`"),
            (
                edited("\"version\":1}", "\"version\":1,\"extra\":true}"),
                "unknown field `extra`",
            ),
            (
                edited("\"version\":1}", "\"version\":1,\"version\":1}"),
                "duplicate field `version`",
            ),
            (
                edited("\"home\":\"share\"", "\"home\":\"nfs\""),
                "unknown home storage \"nfs\"",
            ),
            (
                edited("\"home\":\"share\"", "\"home\":true"),
                "invalid type: boolean",
            ),
            (
                edited(
                    "\"controlSocket\":\"control.sock\"",
                    "\"controlSocket\":\"x.sock\"",
                ),
                "unknown socket name \"x.sock\"",
            ),
            (
                edited(
                    "\"controlSocket\":\"control.sock\"",
                    "\"controlSocket\":\"../control.sock\"",
                ),
                "unknown socket name",
            ),
            (
                edited(
                    "\"controlSocket\":\"control.sock\"",
                    "\"controlSocket\":\"d0.sock\"",
                ),
                "unknown socket name",
            ),
            (
                edited("\"controlSocket\":\"control.sock\"", "\"controlSocket\":7"),
                "invalid type: integer",
            ),
            (
                edited("\"socket\":\"state.sock\"", "\"socket\":\"\""),
                "unknown socket name \"\"",
            ),
            (
                edited("\"tag\":\"amoeboid\"", "\"tag\":\"\""),
                "bad share tag: it is empty",
            ),
            (
                edited("\"tag\":\"amoeboid\"", "\"tag\":\"a b\""),
                "bad share tag",
            ),
            (
                edited("\"mountPoint\":\"/mnt/amoeboid\"", "\"mountPoint\":\"mnt\""),
                "bad guest path: it isn't absolute",
            ),
            (
                edited(
                    "\"mountPoint\":\"/mnt/amoeboid\"",
                    "\"mountPoint\":\"/a\\u0000\"",
                ),
                "control character",
            ),
            (
                edited(
                    "\"extraArgsFile\":\"amoeboid-qemu-args\"",
                    "\"extraArgsFile\":\"../args\"",
                ),
                "bad state file name",
            ),
            (
                edited(
                    "\"extraArgsFile\":\"amoeboid-qemu-args\"",
                    "\"extraArgsFile\":\"..\"",
                ),
                "bad state file name",
            ),
            (edited("\"mounts\",", "\"a/b\","), "bad state file name"),
            (edited("[\"mounts\"", "\"mounts\""), "invalid type: string"),
            (
                edited("\"guestSshPort\":22", "\"guestSshPort\":0"),
                "0 is not a port",
            ),
            (
                edited("\"guestSshPort\":22", "\"guestSshPort\":65536"),
                "invalid value",
            ),
            (
                edited("\"guestSshPort\":22", "\"guestSshPort\":\"22\""),
                "invalid type: string",
            ),
            (
                edited("\"storeShare\":{", "\"storeShare\":{\"x\":1,"),
                "unknown field `x`",
            ),
            (
                edited("\"stateShare\":{", "\"stateShare\":{\"x\":1,"),
                "unknown field `x`",
            ),
            (
                edited(
                    "\"storeShare\":{\"socket\":\"ro-store.sock\",",
                    "\"storeShare\":{",
                ),
                "missing field `socket`",
            ),
            (
                br#"{"\u001b[31mx":1}"#.to_vec(),
                "unknown field `\\u{1b}[31mx`",
            ),
        ];
        for (index, (bytes, needle)) in cases.iter().enumerate() {
            let message = invalid(bytes);
            assert!(
                message.contains(needle),
                "case {index}: {message:?} lacks {needle:?}"
            );
            assert!(!message.contains('\u{1b}'), "case {index}: {message:?}");
        }
    }

    #[test]
    fn a_contract_of_another_version_still_parses_and_is_then_a_version_mismatch() {
        let contract = Contract::parse(&edited("\"version\":1", "\"version\":2")).unwrap();
        assert!(matches!(
            check(&contract),
            Err(BuildError::ContractMismatch {
                field: ContractField::Version,
                ..
            })
        ));
    }
}
