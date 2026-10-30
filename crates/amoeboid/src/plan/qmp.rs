//! The QMP messages amoeboid exchanges with qemu's control socket: the two commands it sends and
//! the greeting and replies it accepts (`docs/interop/qmp-spec`). Pure: lines in, typed values out;
//! the socket is the adapter's.

use std::fmt;

use serde_json::{Map, Value};

use crate::diagnostic::Diagnostic;

/// The commands sent, in order: negotiate, then press the power button.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum QmpCommand {
    Capabilities,
    Powerdown,
}

impl QmpCommand {
    /// The `execute` name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Capabilities => "qmp_capabilities",
            Self::Powerdown => "system_powerdown",
        }
    }

    /// `{"execute":"NAME"}` and a newline.
    #[must_use]
    pub fn line(self) -> QmpLine {
        let mut text = serde_json::json!({ "execute": self.name() }).to_string();
        text.push('\n');
        QmpLine(text)
    }
}

impl fmt::Display for QmpCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// One command, ready to write to the socket.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct QmpLine(String);

impl QmpLine {
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

/// qemu's hello: proof that the peer speaks QMP.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct QmpGreeting(());

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum GreetingError {
    #[error("qemu's greeting isn't JSON")]
    NotJson,
    #[error("qemu's greeting isn't a QMP greeting")]
    NotQmp,
}

impl Diagnostic for GreetingError {
    fn code(&self) -> &'static str {
        match self {
            Self::NotJson => "qmp.greeting.not-json",
            Self::NotQmp => "qmp.greeting.not-qmp",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some("check that the control socket belongs to this VM's qemu".to_owned())
    }
}

impl QmpGreeting {
    /// # Errors
    /// Text that is not JSON, or JSON that is not `{"QMP": {...}}`.
    pub fn parse(line: &[u8]) -> Result<Self, GreetingError> {
        match serde_json::from_slice::<Value>(line) {
            Err(_) => Err(GreetingError::NotJson),
            Ok(Value::Object(fields)) if matches!(fields.get("QMP"), Some(Value::Object(_))) => {
                Ok(Self(()))
            }
            Ok(_) => Err(GreetingError::NotQmp),
        }
    }
}

/// The error classes of the QMP specification.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum QmpErrorClass {
    GenericError,
    CommandNotFound,
    DeviceNotActive,
    DeviceNotFound,
    KvmMissingCap,
}

impl QmpErrorClass {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GenericError => "GenericError",
            Self::CommandNotFound => "CommandNotFound",
            Self::DeviceNotActive => "DeviceNotActive",
            Self::DeviceNotFound => "DeviceNotFound",
            Self::KvmMissingCap => "KVMMissingCap",
        }
    }

    fn named(name: &str) -> Option<Self> {
        [
            Self::GenericError,
            Self::CommandNotFound,
            Self::DeviceNotActive,
            Self::DeviceNotFound,
            Self::KvmMissingCap,
        ]
        .into_iter()
        .find(|class| class.as_str() == name)
    }
}

impl fmt::Display for QmpErrorClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Longest description kept, in characters.
pub const MAX_DESC: usize = 256;

/// qemu's description of an error, safe to show: control characters are dropped and the text is
/// cut at [`MAX_DESC`] characters. Only [`QmpDesc::of`] makes one.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct QmpDesc(String);

impl QmpDesc {
    #[must_use]
    pub fn of(text: &str) -> Self {
        Self(
            text.chars()
                .filter(|ch| !ch.is_control())
                .take(MAX_DESC)
                .collect(),
        )
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for QmpDesc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One line from qemu after the greeting.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum QmpReply {
    /// The command succeeded (its value is not used).
    Return,
    /// An asynchronous event (`SHUTDOWN`, `POWERDOWN`, ...); not an answer.
    Event,
    Error {
        class: QmpErrorClass,
        desc: QmpDesc,
    },
}

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum ReplyError {
    #[error("qemu's reply isn't JSON")]
    NotJson,
    /// Not an object with exactly one of `return`, `error` and `event`.
    #[error("qemu's reply is neither a return, an error nor an event")]
    NotMessage,
    #[error("qemu's error reply has no class and description")]
    BadError,
    /// `class` is cut at 32 characters.
    #[error("qemu's error reply has an unknown class {class:?}")]
    UnknownClass { class: String },
}

impl Diagnostic for ReplyError {
    fn code(&self) -> &'static str {
        match self {
            Self::NotJson => "qmp.reply.not-json",
            Self::NotMessage => "qmp.reply.not-message",
            Self::BadError => "qmp.reply.bad-error",
            Self::UnknownClass { .. } => "qmp.reply.unknown-class",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some("check that the control socket belongs to this VM's qemu".to_owned())
    }
}

impl QmpReply {
    /// # Errors
    /// Text that is not JSON, JSON that is not a QMP message, or an error without a known class
    /// and a description.
    pub fn parse(line: &[u8]) -> Result<Self, ReplyError> {
        let value = serde_json::from_slice::<Value>(line).map_err(|_| ReplyError::NotJson)?;
        let Value::Object(fields) = value else {
            return Err(ReplyError::NotMessage);
        };
        match (
            fields.get("return"),
            fields.get("error"),
            fields.get("event"),
        ) {
            (Some(_), None, None) => Ok(Self::Return),
            (None, None, Some(_)) => Ok(Self::Event),
            (None, Some(error), None) => Self::error(error),
            _ => Err(ReplyError::NotMessage),
        }
    }

    fn error(error: &Value) -> Result<Self, ReplyError> {
        let text = |key: &str| {
            error
                .as_object()
                .and_then(|fields: &Map<String, Value>| fields.get(key))
                .and_then(Value::as_str)
        };
        let (Some(class), Some(desc)) = (text("class"), text("desc")) else {
            return Err(ReplyError::BadError);
        };
        match QmpErrorClass::named(class) {
            Some(class) => Ok(Self::Error {
                class,
                desc: QmpDesc::of(desc),
            }),
            None => Err(ReplyError::UnknownClass {
                class: class.chars().take(32).collect(),
            }),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::testing::{self, Entry};

    fn greeting_witness(e: &GreetingError) -> &'static str {
        match e {
            GreetingError::NotJson => "NotJson",
            GreetingError::NotQmp => "NotQmp",
        }
    }

    fn reply_witness(e: &ReplyError) -> &'static str {
        match e {
            ReplyError::NotJson => "NotJson",
            ReplyError::NotMessage => "NotMessage",
            ReplyError::BadError => "BadError",
            ReplyError::UnknownClass { .. } => "UnknownClass",
        }
    }

    fn greeting_errors() -> Vec<GreetingError> {
        vec![GreetingError::NotJson, GreetingError::NotQmp]
    }

    fn reply_errors() -> Vec<ReplyError> {
        vec![
            ReplyError::NotJson,
            ReplyError::NotMessage,
            ReplyError::BadError,
            ReplyError::UnknownClass {
                class: "Odd".to_owned(),
            },
        ]
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        let greeting = greeting_errors().iter().map(Entry::of).collect::<Vec<_>>();
        let reply = reply_errors().iter().map(Entry::of).collect::<Vec<_>>();
        [greeting, reply].concat()
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(&greeting_errors(), greeting_witness, &["NotJson", "NotQmp"]);
        testing::every_variant(
            &reply_errors(),
            reply_witness,
            &["NotJson", "NotMessage", "BadError", "UnknownClass"],
        );
    }

    #[test]
    fn commands_are_one_json_line_each() {
        let cases = [
            (
                QmpCommand::Capabilities,
                "{\"execute\":\"qmp_capabilities\"}\n",
            ),
            (
                QmpCommand::Powerdown,
                "{\"execute\":\"system_powerdown\"}\n",
            ),
        ];
        for (command, expected) in cases {
            assert_eq!(command.to_string(), command.name());
            assert_eq!(command.line().as_bytes(), expected.as_bytes());
        }
    }

    #[test]
    fn greetings_are_qmp_objects_and_everything_else_is_a_typed_error() {
        let real = br#"{"QMP": {"version": {"qemu": {"micro": 0, "minor": 2, "major": 9}, "package": ""}, "capabilities": ["oob"]}}"#;
        testing::table(
            &[
                (real.as_slice(), Ok(())),
                (b"{\"QMP\":{}}\n".as_slice(), Ok(())),
                (b"".as_slice(), Err(GreetingError::NotJson)),
                (b"hello".as_slice(), Err(GreetingError::NotJson)),
                (b"\xff\xfe".as_slice(), Err(GreetingError::NotJson)),
                (b"{\"QMP\":".as_slice(), Err(GreetingError::NotJson)),
                (b"[]".as_slice(), Err(GreetingError::NotQmp)),
                (b"null".as_slice(), Err(GreetingError::NotQmp)),
                (b"{}".as_slice(), Err(GreetingError::NotQmp)),
                (b"{\"QMP\":1}".as_slice(), Err(GreetingError::NotQmp)),
                (b"{\"qmp\":{}}".as_slice(), Err(GreetingError::NotQmp)),
                (b"{\"return\":{}}".as_slice(), Err(GreetingError::NotQmp)),
            ],
            |line| QmpGreeting::parse(line).map(|_| ()),
        );
    }

    fn error(class: QmpErrorClass, desc: &str) -> QmpReply {
        QmpReply::Error {
            class,
            desc: QmpDesc::of(desc),
        }
    }

    #[test]
    fn replies_are_returns_events_or_errors_and_everything_else_is_a_typed_error() {
        testing::table(
            &[
                (b"{\"return\": {}}".as_slice(), Ok(QmpReply::Return)),
                (b"{\"return\": null}\n".as_slice(), Ok(QmpReply::Return)),
                (b"{\"return\":{},\"id\":1}".as_slice(), Ok(QmpReply::Return)),
                (
                    br#"{"event": "SHUTDOWN", "data": {"guest": true}, "timestamp": {"seconds": 1, "microseconds": 2}}"#
                        .as_slice(),
                    Ok(QmpReply::Event),
                ),
                (
                    br#"{"error": {"class": "GenericError", "desc": "boom"}}"#.as_slice(),
                    Ok(error(QmpErrorClass::GenericError, "boom")),
                ),
                (b"nope".as_slice(), Err(ReplyError::NotJson)),
                (b"".as_slice(), Err(ReplyError::NotJson)),
                (b"[1]".as_slice(), Err(ReplyError::NotMessage)),
                (b"3".as_slice(), Err(ReplyError::NotMessage)),
                (b"{}".as_slice(), Err(ReplyError::NotMessage)),
                (b"{\"x\":1}".as_slice(), Err(ReplyError::NotMessage)),
                (b"{\"return\":1,\"event\":\"A\"}".as_slice(), Err(ReplyError::NotMessage)),
                (
                    br#"{"return":1,"error":{"class":"GenericError","desc":"x"}}"#.as_slice(),
                    Err(ReplyError::NotMessage),
                ),
                (b"{\"event\":\"A\",\"error\":1}".as_slice(), Err(ReplyError::NotMessage)),
                (b"{\"error\":1}".as_slice(), Err(ReplyError::BadError)),
                (b"{\"error\":{}}".as_slice(), Err(ReplyError::BadError)),
                (b"{\"error\":{\"class\":\"GenericError\"}}".as_slice(), Err(ReplyError::BadError)),
                (b"{\"error\":{\"desc\":\"x\"}}".as_slice(), Err(ReplyError::BadError)),
                (b"{\"error\":{\"class\":1,\"desc\":\"x\"}}".as_slice(), Err(ReplyError::BadError)),
                (
                    br#"{"error":{"class":"Weird","desc":"x"}}"#.as_slice(),
                    Err(ReplyError::UnknownClass {
                        class: "Weird".to_owned(),
                    }),
                ),
            ],
            |line| QmpReply::parse(line),
        );
    }

    #[test]
    fn every_error_class_is_recognised_by_its_spec_name() {
        testing::table(
            &[
                ("GenericError", QmpErrorClass::GenericError),
                ("CommandNotFound", QmpErrorClass::CommandNotFound),
                ("DeviceNotActive", QmpErrorClass::DeviceNotActive),
                ("DeviceNotFound", QmpErrorClass::DeviceNotFound),
                ("KVMMissingCap", QmpErrorClass::KvmMissingCap),
            ],
            |name| {
                let class = QmpErrorClass::named(name).unwrap();
                assert_eq!(class.as_str(), *name);
                assert_eq!(class.to_string(), *name);
                class
            },
        );
        assert_eq!(QmpErrorClass::named("kvmmissingcap"), None);
    }

    #[test]
    fn a_description_loses_control_characters_and_is_cut_at_the_limit() {
        let long = "x".repeat(MAX_DESC + 10);
        testing::table(
            &[
                ("plain text", "plain text".to_owned()),
                ("a\u{1b}[31mred\nline\u{7f}", "a[31mredline".to_owned()),
                ("\u{80}\u{9b}x", "x".to_owned()),
                ("é日本 ok", "é日本 ok".to_owned()),
                (long.as_str(), "x".repeat(MAX_DESC)),
            ],
            |text| {
                let desc = QmpDesc::of(text);
                assert_eq!(desc.to_string(), desc.as_str());
                desc.as_str().to_owned()
            },
        );
    }

    #[test]
    fn an_unknown_class_is_cut_in_the_error() {
        let line = format!(
            r#"{{"error":{{"class":"{}","desc":"x"}}}}"#,
            "C".repeat(100)
        );
        assert_eq!(
            QmpReply::parse(line.as_bytes()),
            Err(ReplyError::UnknownClass {
                class: "C".repeat(32)
            })
        );
    }
}
