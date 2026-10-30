//! `RunConfig` -> `Document` -> canonical text.

use crate::config::problem::Key;
use crate::config::run_config::{HEADER, RunConfig, SshConfig, TimeoutsConfig};
use crate::domain::{BaseSpec, MountRequest, SshPort};
use crate::kdl::{self, Document, Int, Value};

/// The document of `config`: every key is written, defaults included, in a fixed order.
#[must_use]
pub fn encode(config: &RunConfig) -> Document {
    document([
        (Key::User, Value::Str(config.user.get().clone())),
        (Key::Uid, int(*config.uid.get())),
        (Key::Gid, int(*config.gid.get())),
        (Key::BaseNixConfig, base(&config.base_nix_config)),
        (
            Key::DefaultMounts,
            Value::List(config.default_mounts.iter().map(mount).collect()),
        ),
        (
            Key::HomeFiles,
            Value::List(
                config
                    .home_files
                    .iter()
                    .map(|rel| Value::Str(rel.get().clone()))
                    .collect(),
            ),
        ),
        (Key::Ssh, ssh(config.ssh)),
        (Key::Timeouts, timeouts(config.timeouts)),
        (Key::Linger, text(config.linger.as_str())),
        (Key::HomeStorage, text(config.home_storage.as_str())),
    ])
}

/// The text of `config`: [`HEADER`], a blank line, the canonical document.
#[must_use]
pub fn render(config: &RunConfig) -> String {
    format!("{HEADER}\n{}", kdl::emit(&encode(config)))
}

fn text(text: &str) -> Value {
    Value::Str(text.to_owned())
}

fn int(number: u32) -> Value {
    Value::Int(Int::from_i64(i64::from(number)))
}

/// The keys of each call site are distinct constants, so no entry replaces another.
fn document<const N: usize>(entries: [(Key, Value); N]) -> Document {
    let mut doc = Document::new();
    for (key, value) in entries {
        doc.set(key.as_str(), value);
    }
    doc
}

fn dict<const N: usize>(entries: [(Key, Value); N]) -> Value {
    Value::Dict(document(entries))
}

fn base(spec: &BaseSpec) -> Value {
    match spec {
        BaseSpec::Preset(name) => Value::Str(name.get().clone()),
        BaseSpec::Dir(dir) => Value::Str(dir.get().clone()),
    }
}

fn mount(request: &MountRequest) -> Value {
    dict([
        (Key::Path, Value::Str(request.path.get().clone())),
        (Key::Mode, text(request.mode.as_str())),
    ])
}

fn ssh(config: SshConfig) -> Value {
    let port = match config.port {
        SshPort::Auto => 0,
        SshPort::Fixed(port) => u32::from(*port.get()),
    };
    dict([(Key::Port, int(port))])
}

fn timeouts(config: TimeoutsConfig) -> Value {
    dict([
        (Key::Boot, int(*config.boot.get())),
        (Key::Stop, int(*config.stop.get())),
    ])
}
