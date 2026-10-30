//! `Run`: the processes of a running VM, `<data>/NAME/run` (KDL, design §Run record).
//!
//! Written by the lock holder once everything is up, read by `stop`, `status` and the next
//! `start` (to classify a held lock). The runtime directory is not in the file: it is a function
//! of the environment and the VM name, and only `Paths::vm` can prove its socket paths fit, so
//! [`Run::parse`] takes it from the same [`VmPaths`] the file's `vm` must agree with.

use crate::config::ValueKind;
use crate::diagnostic::Diagnostic;
use crate::domain::{
    AbsPath, AbsPathError, ContractVersion, ContractVersionError, LoopbackPort, LoopbackPortError,
    Pid, PidError, ProcIdentity, ProgramName, ProgramNameError, StartTime, VmName, VmNameError,
};
use crate::kdl::{self, Document, Int, ParseError, Value};
use crate::layout::{RuntimeDir, VmPaths};
use crate::plan::ShareRole;

const VM: &str = "vm";
const QEMU: &str = "qemu";
const HELPERS: &str = "helpers";
const SSH: &str = "ssh";
const CONTRACT: &str = "contract";
const CONSOLE: &str = "console";
const ROLE: &str = "role";
const PID: &str = "pid";
const START: &str = "start";
const PROGRAM: &str = "program";

/// A running (or dangling) VM. Fields are private: the helpers' roles are distinct, and `vm` and
/// `runtime` belong together, which only [`Run::new`] and [`Run::parse`] establish.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Run {
    vm: VmName,
    runtime: RuntimeDir,
    qemu: ProcIdentity,
    helpers: Vec<(ShareRole, ProcIdentity)>,
    ssh: LoopbackPort,
    contract: ContractVersion,
    console: Option<AbsPath>,
}

/// What is wrong with a run record. `at` names the key (`qemu.pid`, `helpers[1].role`).
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum RunError {
    #[error(transparent)]
    Syntax(ParseError),
    #[error("{at}: missing")]
    Missing { at: String },
    #[error("{at}: expected {}, found {}", .expected.label(), .found.label())]
    Type {
        at: String,
        expected: ValueKind,
        found: ValueKind,
    },
    #[error("{at}: unknown key")]
    Unknown { at: String },
    #[error("{at}: {value} is out of range")]
    Range { at: String, value: i128 },
    #[error("{at}: {error}")]
    Vm { at: String, error: VmNameError },
    #[error("{VM}: the record is of '{recorded}', not '{expected}'")]
    WrongVm { recorded: VmName, expected: VmName },
    #[error("{at}: {error}")]
    Pid { at: String, error: PidError },
    #[error("{at}: {error}")]
    Program { at: String, error: ProgramNameError },
    #[error("{at}: {found:?} is not a share role")]
    Role { at: String, found: String },
    #[error("{SSH}: {0}")]
    Port(LoopbackPortError),
    #[error("{CONTRACT}: {0}")]
    Contract(ContractVersionError),
    #[error("{CONSOLE}: {0}")]
    Console(AbsPathError),
    #[error("two helpers have the role {role}")]
    DuplicateHelper { role: ShareRole },
}

impl Diagnostic for RunError {
    fn code(&self) -> &'static str {
        match self {
            Self::Syntax(_) => "run-record.syntax",
            Self::Missing { .. } => "run-record.missing",
            Self::Type { .. } => "run-record.type",
            Self::Unknown { .. } => "run-record.unknown-key",
            Self::Range { .. } => "run-record.range",
            Self::Vm { .. } => "run-record.vm",
            Self::WrongVm { .. } => "run-record.wrong-vm",
            Self::Pid { .. } => "run-record.pid",
            Self::Program { .. } => "run-record.program",
            Self::Role { .. } => "run-record.role",
            Self::Port(_) => "run-record.port",
            Self::Contract(_) => "run-record.contract",
            Self::Console(_) => "run-record.console",
            Self::DuplicateHelper { .. } => "run-record.duplicate-helper",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::Syntax(error) => error.hint(),
            Self::Missing { .. }
            | Self::Type { .. }
            | Self::Unknown { .. }
            | Self::Range { .. }
            | Self::Vm { .. }
            | Self::WrongVm { .. }
            | Self::Pid { .. }
            | Self::Program { .. }
            | Self::Role { .. }
            | Self::Port(_)
            | Self::Contract(_)
            | Self::Console(_)
            | Self::DuplicateHelper { .. } => {
                Some("delete the file once no process of the VM is left".to_owned())
            }
        }
    }
}

impl Run {
    /// The record of the VM of `paths`, whose runtime directory it also names.
    ///
    /// # Errors
    ///
    /// Two helpers with the same role.
    pub fn new(
        paths: &VmPaths,
        qemu: ProcIdentity,
        helpers: Vec<(ShareRole, ProcIdentity)>,
        ssh: LoopbackPort,
        contract: ContractVersion,
        console: Option<AbsPath>,
    ) -> Result<Self, RunError> {
        for (n, (role, _)) in helpers.iter().enumerate() {
            if helpers
                .iter()
                .skip(n.saturating_add(1))
                .any(|(r, _)| r == role)
            {
                return Err(RunError::DuplicateHelper { role: *role });
            }
        }
        Ok(Self {
            vm: paths.name().clone(),
            runtime: paths.runtime().clone(),
            qemu,
            helpers,
            ssh,
            contract,
            console,
        })
    }

    /// Reads the text of the record of the VM of `paths`.
    ///
    /// # Errors
    ///
    /// Not KDL, a missing, mistyped, out-of-range or unknown key, a record of another VM, or
    /// two helpers with the same role; the first problem found.
    pub fn parse(text: &str, paths: &VmPaths) -> Result<Self, RunError> {
        let doc = kdl::parse(text).map_err(RunError::Syntax)?;
        no_unknown(&doc, "", &[VM, QEMU, HELPERS, SSH, CONTRACT, CONSOLE])?;
        let (value, at) = field(&doc, "", VM)?;
        let recorded = VmName::parse(text_of(value, &at)?.to_owned())
            .map_err(|error| RunError::Vm { at, error })?;
        if recorded != *paths.name() {
            return Err(RunError::WrongVm {
                recorded,
                expected: paths.name().clone(),
            });
        }
        let (value, at) = field(&doc, "", QEMU)?;
        let qemu = process(block(value, &at)?, &at, &[PID, START, PROGRAM])?;
        let (value, at) = field(&doc, "", HELPERS)?;
        let helpers = items(value, &at)?
            .iter()
            .enumerate()
            .map(|(n, item)| helper(item, &format!("{at}[{n}]")))
            .collect::<Result<Vec<_>, _>>()?;
        let (value, at) = field(&doc, "", SSH)?;
        let ssh = LoopbackPort::parse(narrow(int(value, &at)?, &at)?).map_err(RunError::Port)?;
        let (value, at) = field(&doc, "", CONTRACT)?;
        let contract =
            ContractVersion::parse(narrow(int(value, &at)?, &at)?).map_err(RunError::Contract)?;
        let console = doc
            .get(CONSOLE)
            .map(|value| {
                AbsPath::parse(text_of(value, CONSOLE)?.to_owned()).map_err(RunError::Console)
            })
            .transpose()?;
        Self::new(paths, qemu, helpers, ssh, contract, console)
    }

    /// The canonical document of the record.
    #[must_use]
    pub fn encode(&self) -> Document {
        let mut doc = Document::new();
        doc.set(VM, Value::Str(self.vm.get().clone()));
        doc.set(QEMU, Value::Dict(identity(&self.qemu)));
        let helpers = self.helpers.iter().map(|(role, process)| {
            let mut item = Document::new();
            item.set(ROLE, Value::Str(role.to_string()));
            for (key, value) in identity(process) {
                item.set(key, value);
            }
            Value::Dict(item)
        });
        doc.set(HELPERS, Value::List(helpers.collect()));
        doc.set(SSH, number(u32::from(*self.ssh.get())));
        doc.set(CONTRACT, number(*self.contract.get()));
        if let Some(console) = &self.console {
            doc.set(CONSOLE, Value::Str(console.get().clone()));
        }
        doc
    }

    /// The text of the record: [`Run::encode`] as KDL.
    #[must_use]
    pub fn render(&self) -> String {
        kdl::emit(&self.encode())
    }

    #[must_use]
    pub fn vm(&self) -> &VmName {
        &self.vm
    }

    #[must_use]
    pub fn runtime(&self) -> &RuntimeDir {
        &self.runtime
    }

    #[must_use]
    pub fn qemu(&self) -> &ProcIdentity {
        &self.qemu
    }

    /// The virtiofsd instances, in the order they were started.
    #[must_use]
    pub fn helpers(&self) -> &[(ShareRole, ProcIdentity)] {
        &self.helpers
    }

    #[must_use]
    pub fn ssh(&self) -> LoopbackPort {
        self.ssh
    }

    #[must_use]
    pub fn contract(&self) -> ContractVersion {
        self.contract
    }

    /// The console log being written, when the VM runs detached.
    #[must_use]
    pub fn console(&self) -> Option<&AbsPath> {
        self.console.as_ref()
    }
}

fn number(value: u32) -> Value {
    Value::Int(Int::from_i64(i64::from(value)))
}

fn identity(process: &ProcIdentity) -> Document {
    let mut doc = Document::new();
    doc.set(PID, number(*process.pid.get()));
    doc.set(START, Value::Int(Int::from_u64(*process.start.get())));
    doc.set(PROGRAM, Value::Str(process.program.get().clone()));
    doc
}

fn child(parent: &str, key: &str) -> String {
    if parent.is_empty() {
        key.to_owned()
    } else {
        format!("{parent}.{key}")
    }
}

/// The value of `key` below `parent` and its name for messages.
fn field<'a>(doc: &'a Document, parent: &str, key: &str) -> Result<(&'a Value, String), RunError> {
    let at = child(parent, key);
    match doc.get(key) {
        Some(value) => Ok((value, at)),
        None => Err(RunError::Missing { at }),
    }
}

fn no_unknown(doc: &Document, parent: &str, known: &[&str]) -> Result<(), RunError> {
    match doc.iter().find(|(key, _)| !known.contains(key)) {
        Some((key, _)) => Err(RunError::Unknown {
            at: child(parent, key),
        }),
        None => Ok(()),
    }
}

fn mistyped(at: &str, expected: ValueKind, found: &Value) -> RunError {
    RunError::Type {
        at: at.to_owned(),
        expected,
        found: ValueKind::of(found),
    }
}

fn text_of<'a>(value: &'a Value, at: &str) -> Result<&'a str, RunError> {
    if let Value::Str(text) = value {
        return Ok(text);
    }
    Err(mistyped(at, ValueKind::Str, value))
}

fn int(value: &Value, at: &str) -> Result<i128, RunError> {
    if let Value::Int(number) = value {
        return Ok(*number.get());
    }
    Err(mistyped(at, ValueKind::Int, value))
}

fn block<'a>(value: &'a Value, at: &str) -> Result<&'a Document, RunError> {
    if let Value::Dict(doc) = value {
        return Ok(doc);
    }
    Err(mistyped(at, ValueKind::Dict, value))
}

fn items<'a>(value: &'a Value, at: &str) -> Result<&'a [Value], RunError> {
    if let Value::List(items) = value {
        return Ok(items);
    }
    Err(mistyped(at, ValueKind::List, value))
}

fn narrow<T: TryFrom<i128>>(value: i128, at: &str) -> Result<T, RunError> {
    T::try_from(value).map_err(|_| RunError::Range {
        at: at.to_owned(),
        value,
    })
}

/// The process described by `doc`, which may hold other `known` keys (a helper's role).
fn process(doc: &Document, at: &str, known: &[&str]) -> Result<ProcIdentity, RunError> {
    no_unknown(doc, at, known)?;
    let (value, key) = field(doc, at, PID)?;
    let pid = Pid::parse(narrow(int(value, &key)?, &key)?)
        .map_err(|error| RunError::Pid { at: key, error })?;
    let (value, key) = field(doc, at, START)?;
    let start = StartTime::parse(narrow(int(value, &key)?, &key)?).map_err(|e| match e {})?;
    let (value, key) = field(doc, at, PROGRAM)?;
    let program = ProgramName::parse(text_of(value, &key)?.to_owned())
        .map_err(|error| RunError::Program { at: key, error })?;
    Ok(ProcIdentity {
        pid,
        start,
        program,
    })
}

fn helper(value: &Value, at: &str) -> Result<(ShareRole, ProcIdentity), RunError> {
    let doc = block(value, at)?;
    let (role, key) = field(doc, at, ROLE)?;
    let found = text_of(role, &key)?;
    let role = ShareRole::parse(found).ok_or_else(|| RunError::Role {
        at: key,
        found: found.to_owned(),
    })?;
    Ok((role, process(doc, at, &[ROLE, PID, START, PROGRAM])?))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use std::num::NonZeroU16;

    use super::*;
    use crate::domain::{ShareTag, Uid};
    use crate::layout::{Env, Paths, Var};
    use crate::testing::{self, Entry};

    fn paths_of(name: &str) -> VmPaths {
        let var = |text: &str| Var::Text(text.to_owned());
        let env = Env {
            home: var("/home/alice"),
            xdg_runtime_dir: var("/run/user/1000"),
            ..Env::UNSET
        };
        Paths::resolve(&env, Uid::parse(1000).unwrap())
            .unwrap()
            .vm(&VmName::parse(name.to_owned()).unwrap())
    }

    fn proc(pid: u32, start: u64, program: &str) -> ProcIdentity {
        ProcIdentity {
            pid: Pid::parse(pid).unwrap(),
            start: StartTime::parse(start).unwrap(),
            program: ProgramName::parse(program.to_owned()).unwrap(),
        }
    }

    fn tag(n: u16) -> ShareRole {
        ShareRole::Dir(ShareTag::nth(NonZeroU16::new(n).unwrap()))
    }

    fn sample(console: bool) -> Run {
        Run::new(
            &paths_of("proj"),
            proc(4242, 987_654, "qemu-system-x86_64"),
            vec![
                (ShareRole::Store, proc(4243, 987_660, "virtiofsd")),
                (ShareRole::State, proc(4244, 987_661, "virtiofsd")),
                (tag(1), proc(4245, u64::MAX, "virtiofsd")),
            ],
            LoopbackPort::parse(2222).unwrap(),
            ContractVersion::parse(3).unwrap(),
            console.then(|| {
                AbsPath::parse("/home/alice/.local/share/amoeboid/proj/console.log".to_owned())
                    .unwrap()
            }),
        )
        .unwrap()
    }

    const TEXT: &str = r#"vm proj
qemu {
    pid 4242
    start 987654
    program qemu-system-x86_64
}
(list)helpers {
    - {
        role virtiofsd-ro-store
        pid 4243
        start 987660
        program virtiofsd
    }
    - {
        role virtiofsd-state
        pid 4244
        start 987661
        program virtiofsd
    }
    - {
        role virtiofsd-d1
        pid 4245
        start 18446744073709551615
        program virtiofsd
    }
}
ssh 2222
contract 3
console "/home/alice/.local/share/amoeboid/proj/console.log"
"#;

    fn witness(e: &RunError) -> &'static str {
        match e {
            RunError::Syntax(_) => "Syntax",
            RunError::Missing { .. } => "Missing",
            RunError::Type { .. } => "Type",
            RunError::Unknown { .. } => "Unknown",
            RunError::Range { .. } => "Range",
            RunError::Vm { .. } => "Vm",
            RunError::WrongVm { .. } => "WrongVm",
            RunError::Pid { .. } => "Pid",
            RunError::Program { .. } => "Program",
            RunError::Role { .. } => "Role",
            RunError::Port(_) => "Port",
            RunError::Contract(_) => "Contract",
            RunError::Console(_) => "Console",
            RunError::DuplicateHelper { .. } => "DuplicateHelper",
        }
    }

    fn name(text: &str) -> VmName {
        VmName::parse(text.to_owned()).unwrap()
    }

    fn errors() -> Vec<RunError> {
        vec![
            RunError::Syntax(kdl::parse("a {").unwrap_err()),
            RunError::Missing {
                at: "qemu.pid".to_owned(),
            },
            RunError::Type {
                at: "ssh".to_owned(),
                expected: ValueKind::Int,
                found: ValueKind::Str,
            },
            RunError::Unknown {
                at: "extra".to_owned(),
            },
            RunError::Range {
                at: "qemu.pid".to_owned(),
                value: -1,
            },
            RunError::Vm {
                at: "vm".to_owned(),
                error: VmNameError::Empty,
            },
            RunError::WrongVm {
                recorded: name("other"),
                expected: name("proj"),
            },
            RunError::Pid {
                at: "qemu.pid".to_owned(),
                error: PidError::Zero,
            },
            RunError::Program {
                at: "qemu.program".to_owned(),
                error: ProgramNameError::Empty,
            },
            RunError::Role {
                at: "helpers[0].role".to_owned(),
                found: "d1".to_owned(),
            },
            RunError::Port(LoopbackPortError::Zero),
            RunError::Contract(ContractVersionError::Zero),
            RunError::Console(AbsPathError::NotAbsolute),
            RunError::DuplicateHelper {
                role: ShareRole::State,
            },
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
            &[
                "Syntax",
                "Missing",
                "Type",
                "Unknown",
                "Range",
                "Vm",
                "WrongVm",
                "Pid",
                "Program",
                "Role",
                "Port",
                "Contract",
                "Console",
                "DuplicateHelper",
            ],
        );
    }

    #[test]
    fn the_record_renders_as_canonical_kdl_and_reads_back_equal() {
        for console in [true, false] {
            let run = sample(console);
            let text = run.render();
            assert_eq!(Run::parse(&text, &paths_of("proj")), Ok(run.clone()));
            assert_eq!(text.contains("console"), console);
            assert_eq!(kdl::emit(&kdl::parse(&text).unwrap()), text);
        }
    }

    #[test]
    fn the_rendered_record_has_the_documented_shape() {
        let run = sample(true);
        let text = run.render();
        let doc = kdl::parse(&text).unwrap();
        assert_eq!(doc, run.encode());
        let keys: Vec<&str> = doc.iter().map(|(key, _)| key).collect();
        assert_eq!(
            keys,
            ["vm", "qemu", "helpers", "ssh", "contract", "console"]
        );
        assert_eq!(
            Run::parse(TEXT, &paths_of("proj")),
            Ok(run),
            "non-canonical layouts are read too"
        );
    }

    #[test]
    fn the_record_exposes_what_it_was_built_from() {
        let run = sample(true);
        let paths = paths_of("proj");
        assert_eq!(run.vm(), paths.name());
        assert_eq!(run.runtime(), paths.runtime());
        assert_eq!(run.qemu(), &proc(4242, 987_654, "qemu-system-x86_64"));
        assert_eq!(run.helpers().len(), 3);
        assert_eq!(run.ssh(), LoopbackPort::parse(2222).unwrap());
        assert_eq!(run.contract(), ContractVersion::parse(3).unwrap());
        assert_eq!(
            run.console().map(|path| path.get().as_str()),
            Some("/home/alice/.local/share/amoeboid/proj/console.log")
        );
        assert_eq!(sample(false).console(), None);
    }

    #[test]
    fn two_helpers_with_one_role_are_refused_whoever_builds_the_record() {
        let helper = |pid| (ShareRole::State, proc(pid, 1, "virtiofsd"));
        let build = |helpers: Vec<(ShareRole, ProcIdentity)>| {
            Run::new(
                &paths_of("proj"),
                proc(1, 1, "qemu"),
                helpers,
                LoopbackPort::parse(2222).unwrap(),
                ContractVersion::parse(1).unwrap(),
                None,
            )
            .map(|run| run.helpers().len())
        };
        testing::table(
            &[
                (vec![], Ok(0)),
                (vec![helper(2)], Ok(1)),
                (vec![helper(2), (tag(1), proc(3, 1, "virtiofsd"))], Ok(2)),
                (
                    vec![helper(2), (tag(1), proc(3, 1, "virtiofsd")), helper(4)],
                    Err(RunError::DuplicateHelper {
                        role: ShareRole::State,
                    }),
                ),
            ],
            |helpers| build(helpers.clone()),
        );
    }

    /// `TEXT` with one line replaced.
    fn with(from: &str, to: &str) -> String {
        assert!(TEXT.contains(from), "{from}");
        TEXT.replacen(from, to, 1)
    }

    fn missing(at: &str) -> RunError {
        RunError::Missing { at: at.to_owned() }
    }

    fn bad_type(at: &str, expected: ValueKind, found: ValueKind) -> RunError {
        RunError::Type {
            at: at.to_owned(),
            expected,
            found,
        }
    }

    fn range(at: &str, value: i128) -> RunError {
        RunError::Range {
            at: at.to_owned(),
            value,
        }
    }

    #[test]
    fn every_corruption_is_a_typed_error_naming_the_key() {
        let parse = |text: &str| Run::parse(text, &paths_of("proj")).map(|_| ());
        testing::table(
            &[
                (String::new(), Err(missing("vm"))),
                (
                    "garbage {".to_owned(),
                    Err(RunError::Syntax(kdl::parse("garbage {").unwrap_err())),
                ),
                (with("vm proj\n", ""), Err(missing("vm"))),
                (with("ssh 2222\n", ""), Err(missing("ssh"))),
                (with("contract 3\n", ""), Err(missing("contract"))),
                (
                    with("(list)helpers {", "(list)helpers2 {"),
                    Err(RunError::Unknown {
                        at: "helpers2".to_owned(),
                    }),
                ),
                (
                    with("vm proj\n", "vm proj\nextra 1\n"),
                    Err(RunError::Unknown {
                        at: "extra".to_owned(),
                    }),
                ),
                (
                    with("vm proj", "vm 1"),
                    Err(bad_type("vm", ValueKind::Str, ValueKind::Int)),
                ),
                (
                    with("vm proj", "vm \"\""),
                    Err(RunError::Vm {
                        at: "vm".to_owned(),
                        error: VmNameError::Empty,
                    }),
                ),
                (
                    with("vm proj", "vm other"),
                    Err(RunError::WrongVm {
                        recorded: name("other"),
                        expected: name("proj"),
                    }),
                ),
                (with("    pid 4242\n", ""), Err(missing("qemu.pid"))),
                (with("    start 987654\n", ""), Err(missing("qemu.start"))),
                (
                    with("    program qemu-system-x86_64\n", ""),
                    Err(missing("qemu.program")),
                ),
                (
                    with("    pid 4242\n", "    pid 4242\n    colour 1\n"),
                    Err(RunError::Unknown {
                        at: "qemu.colour".to_owned(),
                    }),
                ),
                (
                    with("pid 4242", "pid abc"),
                    Err(bad_type("qemu.pid", ValueKind::Int, ValueKind::Str)),
                ),
                (
                    with("pid 4242", "pid 0"),
                    Err(RunError::Pid {
                        at: "qemu.pid".to_owned(),
                        error: PidError::Zero,
                    }),
                ),
                (with("pid 4242", "pid -1"), Err(range("qemu.pid", -1))),
                (
                    with("pid 4242", "pid 4294967296"),
                    Err(range("qemu.pid", 4_294_967_296)),
                ),
                (
                    with("start 987654", "start -1"),
                    Err(range("qemu.start", -1)),
                ),
                (
                    with("start 987654", "start 18446744073709551616"),
                    Err(range("qemu.start", 18_446_744_073_709_551_616)),
                ),
                (
                    with("start 987654", "start x"),
                    Err(bad_type("qemu.start", ValueKind::Int, ValueKind::Str)),
                ),
                (
                    with("program qemu-system-x86_64", "program \"\""),
                    Err(RunError::Program {
                        at: "qemu.program".to_owned(),
                        error: ProgramNameError::Empty,
                    }),
                ),
                (
                    with("program qemu-system-x86_64", "program 1"),
                    Err(bad_type("qemu.program", ValueKind::Str, ValueKind::Int)),
                ),
            ],
            |text| parse(text),
        );
    }

    #[test]
    fn every_corruption_of_the_helpers_and_the_scalars_is_a_typed_error_naming_the_key() {
        let parse = |text: &str| Run::parse(text, &paths_of("proj")).map(|_| ());
        testing::table(
            &[
                (
                    with("role virtiofsd-ro-store", "role ro-store"),
                    Err(RunError::Role {
                        at: "helpers[0].role".to_owned(),
                        found: "ro-store".to_owned(),
                    }),
                ),
                (
                    with("role virtiofsd-state", "role 5"),
                    Err(bad_type("helpers[1].role", ValueKind::Str, ValueKind::Int)),
                ),
                (
                    with("        role virtiofsd-d1\n", ""),
                    Err(missing("helpers[2].role")),
                ),
                (
                    with("        pid 4245\n", ""),
                    Err(missing("helpers[2].pid")),
                ),
                (
                    with("        pid 4245\n", "        pid 4245\n        extra 1\n"),
                    Err(RunError::Unknown {
                        at: "helpers[2].extra".to_owned(),
                    }),
                ),
                (
                    with("role virtiofsd-state", "role virtiofsd-ro-store"),
                    Err(RunError::DuplicateHelper {
                        role: ShareRole::Store,
                    }),
                ),
                (
                    with("ssh 2222", "ssh 0"),
                    Err(RunError::Port(LoopbackPortError::Zero)),
                ),
                (with("ssh 2222", "ssh 65536"), Err(range("ssh", 65_536))),
                (
                    with("ssh 2222", "ssh x"),
                    Err(bad_type("ssh", ValueKind::Int, ValueKind::Str)),
                ),
                (
                    with("contract 3", "contract 0"),
                    Err(RunError::Contract(ContractVersionError::Zero)),
                ),
                (
                    with("contract 3", "contract 4294967296"),
                    Err(range("contract", 4_294_967_296)),
                ),
                (
                    with("contract 3", "contract x"),
                    Err(bad_type("contract", ValueKind::Int, ValueKind::Str)),
                ),
                (
                    with("console \"/home", "console \"home"),
                    Err(RunError::Console(AbsPathError::NotAbsolute)),
                ),
                (
                    with("console \"/home", "console 1\nignored \"/home"),
                    Err(RunError::Unknown {
                        at: "ignored".to_owned(),
                    }),
                ),
            ],
            |text| parse(text),
        );
    }

    #[test]
    fn blocks_and_lists_of_the_wrong_kind_are_type_errors() {
        let parse = |text: &str| Run::parse(text, &paths_of("proj")).map(|_| ());
        let head = "vm proj\nqemu { pid 1; start 1; program q }\n";
        let tail = "ssh 2222\ncontract 3\n";
        testing::table(
            &[
                (
                    format!("vm proj\nqemu 5\n(list)helpers {{}}\n{tail}"),
                    Err(bad_type("qemu", ValueKind::Dict, ValueKind::Int)),
                ),
                (
                    format!("{head}helpers 5\n{tail}"),
                    Err(bad_type("helpers", ValueKind::List, ValueKind::Int)),
                ),
                (
                    format!("{head}helpers {{}}\n{tail}"),
                    Err(bad_type("helpers", ValueKind::List, ValueKind::Dict)),
                ),
                (
                    format!("{head}(list)helpers {{\n- 5\n}}\n{tail}"),
                    Err(bad_type("helpers[0]", ValueKind::Dict, ValueKind::Int)),
                ),
            ],
            |text| parse(text),
        );
    }

    #[test]
    fn a_record_with_no_helpers_and_no_console_is_valid() {
        let text =
            "vm proj\nqemu { pid 1; start 1; program q }\n(list)helpers {}\nssh 2222\ncontract 3\n";
        let run = Run::parse(text, &paths_of("proj")).unwrap();
        assert_eq!(run.helpers(), []);
        assert_eq!(run.console(), None);
    }
}
