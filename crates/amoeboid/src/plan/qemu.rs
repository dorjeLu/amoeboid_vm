//! The extra qemu arguments of a start (`amoeboid-qemu-args` in the runtime dir). The runner
//! word-splits the file, so no argument contains whitespace (design §Security invariants).

use std::fmt;

use crate::diagnostic::Diagnostic;
use crate::domain::socket;
use crate::domain::word::{self, Fragment, Word};
use crate::domain::{GuestPort, LoopbackPort, ShareTag};
use crate::plan::share::ShareList;
use crate::refined;

/// The guest NIC's MAC address.
pub const GUEST_MAC: Word = refined!(Word, "02:00:00:a0:0b:01");
/// The guest's sshd port (contract `guestSshPort`).
pub const GUEST_SSH_PORT: GuestPort = refined!(GuestPort, 22);

const NETDEV: Word = refined!(Word, "-netdev");
const DEVICE: Word = refined!(Word, "-device");
const CHARDEV: Word = refined!(Word, "-chardev");
const USER_NET: Word = refined!(Word, "user,id=net0,hostfwd=tcp:127.0.0.1:");
const FORWARD_TO: Word = refined!(Word, "-:");
const NIC: Word = refined!(Word, "virtio-net-pci,netdev=net0,mac=");
const SOCKET_ID: Word = refined!(Word, "socket,id=amo");
const SOCKET_PATH: Word = refined!(Word, ",path=");
const FS_DEVICE: Word = refined!(Word, "vhost-user-fs-pci,chardev=amo");
const FS_TAG: Word = refined!(Word, ",tag=");

/// One qemu argument: non-empty, no whitespace.
///
/// Not a `Refined`: besides [`QemuArg::parse`] (text from elsewhere) it is made by the total
/// [`QemuArg::of`], which concatenates proven [`Fragment`]s; there is no constructor from free
/// text.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct QemuArg(String);

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum QemuArgError {
    #[error("bad qemu argument: it is empty")]
    Empty,
    /// `at` is a byte offset.
    #[error("bad qemu argument: whitespace at offset {at}")]
    Whitespace { at: usize },
}

impl Diagnostic for QemuArgError {
    fn code(&self) -> &'static str {
        match self {
            Self::Empty => "qemu-arg.empty",
            Self::Whitespace { .. } => "qemu-arg.whitespace",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some("the runner splits its arguments at whitespace; give one word per argument".to_owned())
    }
}

impl QemuArg {
    /// # Errors
    /// Empty, or the first whitespace character.
    pub fn parse(value: String) -> Result<Self, QemuArgError> {
        if value.is_empty() {
            return Err(QemuArgError::Empty);
        }
        match value.char_indices().find(|&(_, ch)| ch.is_whitespace()) {
            Some((at, _)) => Err(QemuArgError::Whitespace { at }),
            None => Ok(Self(value)),
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The fragments' texts joined: non-empty because `first` is, without whitespace because
    /// every fragment is, so it always passes [`QemuArg::parse`].
    #[must_use]
    pub fn of(first: Fragment, rest: &[Fragment]) -> Self {
        let mut text = word::concat(&[first]);
        text.push_str(&word::concat(rest));
        Self(text)
    }
}

/// Shows the text only, like the refined types.
impl fmt::Debug for QemuArg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// The arguments in order.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct QemuArgs(Vec<QemuArg>);

impl QemuArgs {
    #[must_use]
    pub fn as_slice(&self) -> &[QemuArg] {
        &self.0
    }

    /// The file's text: the arguments joined by spaces, then a newline.
    #[must_use]
    pub fn render(&self) -> String {
        let words: Vec<&str> = self.0.iter().map(QemuArg::as_str).collect();
        format!("{}\n", words.join(" "))
    }
}

/// User-mode networking with the guest's sshd forwarded from `127.0.0.1:port`, and one
/// vhost-user-fs device per shared directory on its `dN.sock` (relative: the runner starts in the
/// runtime dir). The store and state shares are wired by `base.nix`.
#[must_use]
pub fn qemu_args(shares: &ShareList, port: LoopbackPort) -> QemuArgs {
    let text = Fragment::Text;
    let net = [
        QemuArg::of(text(NETDEV), &[]),
        QemuArg::of(
            text(USER_NET),
            &[
                Fragment::Port(port),
                text(FORWARD_TO),
                Fragment::Guest(GUEST_SSH_PORT),
            ],
        ),
        QemuArg::of(text(DEVICE), &[]),
        QemuArg::of(text(NIC), &[text(GUEST_MAC)]),
    ];
    let dirs = shares.dirs().flat_map(|(tag, _)| device(tag));
    QemuArgs(net.into_iter().chain(dirs).collect())
}

/// The chardev on the share's socket (`dN.sock`, as `Socket::Share` names it) and the
/// vhost-user-fs device on it.
fn device(tag: ShareTag) -> [QemuArg; 4] {
    let n = Fragment::Number(tag.index());
    [
        QemuArg::of(Fragment::Text(CHARDEV), &[]),
        QemuArg::of(
            Fragment::Text(SOCKET_ID),
            &[
                n,
                Fragment::Text(SOCKET_PATH),
                Fragment::Tag(tag),
                Fragment::Text(socket::SUFFIX),
            ],
        ),
        QemuArg::of(Fragment::Text(DEVICE), &[]),
        QemuArg::of(
            Fragment::Text(FS_DEVICE),
            &[n, Fragment::Text(FS_TAG), Fragment::Tag(tag)],
        ),
    ]
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::domain::{Mode, Socket};
    use crate::plan::fixtures::{list_of, shares, shares_from_golden};
    use crate::testing::{self, Entry};

    fn witness(e: &QemuArgError) -> &'static str {
        match e {
            QemuArgError::Empty => "Empty",
            QemuArgError::Whitespace { .. } => "Whitespace",
        }
    }

    fn errors() -> Vec<QemuArgError> {
        vec![QemuArgError::Empty, QemuArgError::Whitespace { at: 1 }]
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        errors().iter().map(Entry::of).collect()
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(&errors(), witness, &["Empty", "Whitespace"]);
    }

    #[test]
    fn qemu_arg_parse_refuses_empty_text_and_whitespace() {
        testing::table(
            &[
                ("", Err(QemuArgError::Empty)),
                ("-device", Ok(())),
                ("a=b,c:d", Ok(())),
                ("日本", Ok(())),
                (" ", Err(QemuArgError::Whitespace { at: 0 })),
                ("a b", Err(QemuArgError::Whitespace { at: 1 })),
                ("a\tb", Err(QemuArgError::Whitespace { at: 1 })),
                ("é\n", Err(QemuArgError::Whitespace { at: 2 })),
                ("a\u{2003}", Err(QemuArgError::Whitespace { at: 1 })),
            ],
            |text| QemuArg::parse((*text).to_owned()).map(|_| ()),
        );
    }

    fn port(n: u16) -> LoopbackPort {
        LoopbackPort::parse(n).unwrap()
    }

    #[test]
    fn every_built_argument_is_one_word() {
        let paths: Vec<String> = (0..12).map(|i| format!("/srv/m {i}")).collect();
        let mounts: Vec<(&str, Mode)> = paths.iter().map(|p| (p.as_str(), Mode::Rw)).collect();
        let args = qemu_args(&list_of(&shares(&mounts)), port(65_535));
        assert_eq!(args.as_slice().len(), 52);
        for arg in args.as_slice() {
            assert_eq!(QemuArg::parse(arg.as_str().to_owned()), Ok(arg.clone()));
        }
        let tag = ShareTag::nth(std::num::NonZeroU16::MIN);
        let arg = QemuArg::of(Fragment::Text(DEVICE), &[Fragment::Tag(tag)]);
        assert_eq!(format!("{arg:?}"), "\"-deviced1\"");
    }

    #[test]
    fn a_shares_chardev_path_is_the_name_of_its_socket() {
        for socket in crate::domain::socket::tests::every_socket() {
            let Socket::Share(tag) = socket else { continue };
            let args = device(tag);
            let path = format!("path={}", socket.name());
            assert!(args[1].as_str().ends_with(&path), "{args:?}");
        }
    }

    #[test]
    fn the_fixed_arguments_are_the_constants_in_order() {
        let args = qemu_args(&list_of(&shares(&[])), port(2222));
        let got: Vec<&str> = args.as_slice().iter().map(QemuArg::as_str).collect();
        assert_eq!(
            got,
            [
                "-netdev",
                "user,id=net0,hostfwd=tcp:127.0.0.1:2222-:22",
                "-device",
                "virtio-net-pci,netdev=net0,mac=02:00:00:a0:0b:01",
            ]
        );
    }

    #[test]
    fn qemu_args_match_the_python_ones() {
        let cases = testing::golden_cases("qemu_args", "json", "txt").unwrap();
        assert!(cases.len() > 1);
        for case in cases {
            let input: serde_json::Value = serde_json::from_str(&case.input).unwrap();
            let mounts = shares_from_golden(input.get("mounts").unwrap());
            let n = input
                .get("ssh_port")
                .and_then(serde_json::Value::as_u64)
                .unwrap();
            let got = qemu_args(&list_of(&mounts), port(u16::try_from(n).unwrap()));
            assert_eq!(got.render(), case.output, "{}", case.name);
        }
    }
}
