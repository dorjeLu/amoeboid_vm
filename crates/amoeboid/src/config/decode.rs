//! `Document` -> `RunConfig`: every problem in the file is reported at once.

use crate::config::problem::{Block, Key, Location, Problem, ProblemKind, ValueKind};
use crate::config::run_config::{Anchor, RunConfig, SshConfig, TimeoutsConfig, default_base};
use crate::domain::{
    BaseSpec, Gid, HomeRel, HomeStorage, LingerPolicy, Mode, MountRequest, Seconds, SpecPath,
    SshPort, Uid, UserName,
};
use crate::kdl::{Document, Value};
use crate::rails::{NonEmpty, Validated};

type Decoded<T> = Validated<T, Problem>;

/// Decode and validate, reporting every problem at once. Relative text is anchored at `anchor`.
///
/// # Errors
///
/// Every problem found, in document-structure order (unknown keys last).
pub fn decode(doc: &Document, anchor: &Anchor) -> Result<RunConfig, NonEmpty<Problem>> {
    let root = Location::root();
    let user = field(doc, &root, Key::User, None, leaf(user));
    let uid = field(doc, &root, Key::Uid, None, leaf(uid));
    let gid = field(doc, &root, Key::Gid, None, leaf(gid));
    let base = field(
        doc,
        &root,
        Key::BaseNixConfig,
        Some(Validated::Valid(default_base())),
        leaf(|value| base_nix_config(value, anchor)),
    );
    let mounts = field(
        doc,
        &root,
        Key::DefaultMounts,
        Some(Validated::Valid(Vec::new())),
        |value, at| list(value, at, |item, at| mount(item, at, anchor)),
    );
    let home_files = field(
        doc,
        &root,
        Key::HomeFiles,
        Some(Validated::Valid(Vec::new())),
        |value, at| list(value, at, leaf(home_file)),
    );
    let ssh = field(
        doc,
        &root,
        Key::Ssh,
        Some(Validated::Valid(SshConfig::DEFAULT)),
        ssh,
    );
    let timeouts = field(
        doc,
        &root,
        Key::Timeouts,
        Some(Validated::Valid(TimeoutsConfig::DEFAULT)),
        timeouts,
    );
    let linger = field(
        doc,
        &root,
        Key::Linger,
        Some(Validated::Valid(LingerPolicy::default())),
        leaf(choice(LingerPolicy::NAMES, LingerPolicy::parse)),
    );
    let home_storage = field(
        doc,
        &root,
        Key::HomeStorage,
        Some(Validated::Valid(HomeStorage::default())),
        leaf(choice(HomeStorage::NAMES, HomeStorage::parse)),
    );
    user.zip(uid)
        .zip(gid)
        .zip(base)
        .zip(mounts)
        .zip(home_files)
        .zip(ssh)
        .zip(timeouts)
        .zip(linger)
        .zip(home_storage)
        .zip(unknown_keys(doc, &root, Block::Root))
        .map(
            |(
                (
                    (
                        (
                            (
                                (
                                    ((((user, uid), gid), base_nix_config), default_mounts),
                                    home_files,
                                ),
                                ssh,
                            ),
                            timeouts,
                        ),
                        linger,
                    ),
                    home_storage,
                ),
                (),
            )| RunConfig {
                user,
                uid,
                gid,
                base_nix_config,
                default_mounts,
                home_files,
                ssh,
                timeouts,
                linger,
                home_storage,
            },
        )
        .into_result()
}

/// The value of `key` in `doc`, decoded by `decode` at the key's location; `default` stands in
/// for an absent key, and `None` makes the key required.
fn field<T>(
    doc: &Document,
    at: &Location,
    key: Key,
    default: Option<Decoded<T>>,
    decode: impl FnOnce(&Value, &Location) -> Decoded<T>,
) -> Decoded<T> {
    let at = at.key(key);
    match (doc.get(key.as_str()), default) {
        (Some(value), _) => decode(value, &at),
        (None, Some(default)) => default,
        (None, None) => Validated::invalid(Problem {
            at,
            kind: ProblemKind::Missing,
        }),
    }
}

/// Lifts a decoder that has one thing to say about a value to one that reports it where it is.
fn leaf<T>(
    decode: impl Fn(&Value) -> Result<T, ProblemKind>,
) -> impl Fn(&Value, &Location) -> Decoded<T> {
    move |value, at| {
        Validated::from_result(decode(value).map_err(|kind| Problem {
            at: at.clone(),
            kind,
        }))
    }
}

fn problem<T>(at: &Location, kind: ProblemKind) -> Decoded<T> {
    Validated::invalid(Problem {
        at: at.clone(),
        kind,
    })
}

/// A problem per key of `doc` that `block` does not know.
fn unknown_keys(doc: &Document, at: &Location, block: Block) -> Decoded<()> {
    Validated::all(
        doc.iter()
            .filter(|(key, _)| !block.known().contains(key))
            .map(|(key, _)| {
                problem::<()>(
                    at,
                    ProblemKind::UnknownKey {
                        block,
                        key: key.to_owned(),
                    },
                )
            }),
    )
    .map(|_| ())
}

fn string(value: &Value) -> Result<&str, ProblemKind> {
    if let Value::Str(text) = value {
        Ok(text)
    } else {
        Err(ProblemKind::NotString {
            found: ValueKind::of(value),
        })
    }
}

/// An integer that fits `T`; `max` is `T`'s largest value, for the message.
fn unsigned<T: TryFrom<i128>>(value: &Value, max: u32) -> Result<T, ProblemKind> {
    let Value::Int(number) = value else {
        return Err(ProblemKind::NotInteger {
            found: ValueKind::of(value),
        });
    };
    T::try_from(*number.get()).map_err(|_| ProblemKind::OutOfRange {
        value: *number.get(),
        max,
    })
}

/// One of `names`, mapped by `parse`.
fn choice<T>(
    names: &'static [&'static str],
    parse: fn(&str) -> Option<T>,
) -> impl Fn(&Value) -> Result<T, ProblemKind> {
    move |value| {
        let text = string(value)?;
        parse(text).ok_or_else(|| ProblemKind::BadChoice {
            found: text.to_owned(),
            allowed: names,
        })
    }
}

fn user(value: &Value) -> Result<UserName, ProblemKind> {
    UserName::parse(string(value)?.to_owned()).map_err(ProblemKind::User)
}

fn uid(value: &Value) -> Result<Uid, ProblemKind> {
    Uid::parse(unsigned(value, u32::MAX)?).map_err(ProblemKind::Uid)
}

fn gid(value: &Value) -> Result<Gid, ProblemKind> {
    Gid::parse(unsigned(value, u32::MAX)?).map_err(ProblemKind::Gid)
}

/// A preset name stays a name; a path is made absolute against the config dir.
fn base_nix_config(value: &Value, anchor: &Anchor) -> Result<BaseSpec, ProblemKind> {
    BaseSpec::parse(string(value)?, &anchor.config_dir, &anchor.home).map_err(ProblemKind::Base)
}

/// Like `HomeRel::parse` after dropping `.` and empty components, which older files may have.
fn home_file(value: &Value) -> Result<HomeRel, ProblemKind> {
    HomeRel::normalising(string(value)?).map_err(ProblemKind::HomeFile)
}

fn port(value: &Value) -> Result<SshPort, ProblemKind> {
    SshPort::parse(unsigned(value, u32::from(u16::MAX))?).map_err(ProblemKind::Port)
}

fn seconds(value: &Value) -> Result<Seconds, ProblemKind> {
    Seconds::parse(unsigned(value, u32::MAX)?).map_err(ProblemKind::Seconds)
}

/// A (list) node: every item decoded by `item` at `at: item N`.
fn list<T>(
    value: &Value,
    at: &Location,
    item: impl Fn(&Value, &Location) -> Decoded<T>,
) -> Decoded<Vec<T>> {
    if let Value::List(items) = value {
        Validated::all(
            items
                .iter()
                .enumerate()
                .map(|(index, entry)| item(entry, &at.item(index))),
        )
    } else {
        problem(
            at,
            ProblemKind::NotList {
                found: ValueKind::of(value),
            },
        )
    }
}

/// A node with children, read by `inner`.
fn block<T>(
    value: &Value,
    at: &Location,
    kind: Block,
    inner: impl FnOnce(&Document) -> Decoded<T>,
) -> Decoded<T> {
    if let Value::Dict(doc) = value {
        inner(doc)
    } else {
        problem(
            at,
            ProblemKind::NotBlock {
                block: kind,
                found: ValueKind::of(value),
            },
        )
    }
}

fn mount(value: &Value, at: &Location, anchor: &Anchor) -> Decoded<MountRequest> {
    block(value, at, Block::Mount, |doc| {
        let path = field(
            doc,
            at,
            Key::Path,
            None,
            leaf(|value| {
                SpecPath::parse(string(value)?.to_owned())
                    .map_err(ProblemKind::MountPath)?
                    .resolve(&anchor.config_dir, &anchor.home)
                    .map_err(ProblemKind::MountResolve)
            }),
        );
        let mode = field(
            doc,
            at,
            Key::Mode,
            Some(Validated::Valid(Mode::Rw)),
            leaf(choice(Mode::NAMES, Mode::from_suffix)),
        );
        path.zip(mode)
            .zip(unknown_keys(doc, at, Block::Mount))
            .map(|((path, mode), ())| MountRequest { path, mode })
    })
}

fn ssh(value: &Value, at: &Location) -> Decoded<SshConfig> {
    block(value, at, Block::Ssh, |doc| {
        field(
            doc,
            at,
            Key::Port,
            Some(Validated::Valid(SshConfig::DEFAULT.port)),
            leaf(port),
        )
        .zip(unknown_keys(doc, at, Block::Ssh))
        .map(|(port, ())| SshConfig { port })
    })
}

fn timeouts(value: &Value, at: &Location) -> Decoded<TimeoutsConfig> {
    block(value, at, Block::Timeouts, |doc| {
        let boot = field(
            doc,
            at,
            Key::Boot,
            Some(Validated::Valid(TimeoutsConfig::DEFAULT.boot)),
            leaf(seconds),
        );
        let stop = field(
            doc,
            at,
            Key::Stop,
            Some(Validated::Valid(TimeoutsConfig::DEFAULT.stop)),
            leaf(seconds),
        );
        boot.zip(stop)
            .zip(unknown_keys(doc, at, Block::Timeouts))
            .map(|((boot, stop), ())| TimeoutsConfig { boot, stop })
    })
}
