//! `HostPublicKey`: the guest's ssh host key as `ssh-keygen` writes it to the `.pub` file, ed25519
//! only. The host pins it in the VM's `known_hosts`, so what the file says is checked at the
//! boundary: a key of another type or a key that isn't base64 is a typed error, never a line
//! written to `known_hosts`.

use std::fmt;

use crate::diagnostic::Diagnostic;
use crate::refined::{Refined, Refinement};

/// The key types a VM's host key may have. Only ed25519 is generated and shipped to the guest.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum KeyType {
    Ed25519,
}

/// Longest type name quoted in an error; the rest of a hostile token is cut.
const FOUND_MAX: usize = 32;

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum KeyTypeError {
    /// `found` is the type token as written, cut at 32 characters.
    #[error("unsupported host key type {found:?}: only ssh-ed25519 is used")]
    Unsupported { found: String },
}

impl Diagnostic for KeyTypeError {
    fn code(&self) -> &'static str {
        match self {
            Self::Unsupported { .. } => "key-type.unsupported",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some("delete the VM's ssh directory so a new ed25519 key is generated".to_owned())
    }
}

impl KeyType {
    /// The name `ssh-keygen` and `known_hosts` use.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ed25519 => "ssh-ed25519",
        }
    }

    /// # Errors
    /// Any type but `ssh-ed25519`.
    pub fn parse(token: &str) -> Result<Self, KeyTypeError> {
        match token {
            "ssh-ed25519" => Ok(Self::Ed25519),
            other => Err(KeyTypeError::Unsupported {
                found: other.chars().take(FOUND_MAX).collect(),
            }),
        }
    }
}

impl fmt::Display for KeyType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Marker of the base64 text of a public key: the standard alphabet, padded to a multiple of 4.
pub struct Base64Key;

/// The base64 text of a public key blob; proven to be well-formed padded base64 (so it holds no
/// whitespace and can sit in a `known_hosts` line).
pub type KeyBase64 = Refined<String, Base64Key>;

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum KeyBase64Error {
    #[error("bad host key: it is empty")]
    Empty,
    /// `at` counts characters from the start.
    #[error("bad host key: {ch:?} at position {at} isn't base64")]
    BadChar { ch: char, at: usize },
    #[error("bad host key: more than two padding characters")]
    TooMuchPadding,
    #[error("bad host key: its length {len} isn't a multiple of 4")]
    BadLength { len: usize },
}

impl Diagnostic for KeyBase64Error {
    fn code(&self) -> &'static str {
        match self {
            Self::Empty => "key-base64.empty",
            Self::BadChar { .. } => "key-base64.bad-char",
            Self::TooMuchPadding => "key-base64.too-much-padding",
            Self::BadLength { .. } => "key-base64.bad-length",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some("delete the VM's ssh directory so a new key is generated".to_owned())
    }
}

impl Refinement<String> for Base64Key {
    type Error = KeyBase64Error;

    fn check(value: &String) -> Result<(), KeyBase64Error> {
        if value.is_empty() {
            return Err(KeyBase64Error::Empty);
        }
        let body = value.trim_end_matches('=');
        let padding = value.chars().count().saturating_sub(body.chars().count());
        if let Some((at, ch)) = body
            .chars()
            .enumerate()
            .find(|&(_, ch)| !(ch.is_ascii_alphanumeric() || matches!(ch, '+' | '/')))
        {
            return Err(KeyBase64Error::BadChar { ch, at });
        }
        if padding > 2 {
            return Err(KeyBase64Error::TooMuchPadding);
        }
        let len = value.len();
        if !len.is_multiple_of(4) {
            return Err(KeyBase64Error::BadLength { len });
        }
        Ok(())
    }
}

/// A host public key: its type and base64 blob.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct HostPublicKey {
    pub kind: KeyType,
    pub key: KeyBase64,
}

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum HostPublicKeyError {
    #[error("host key file is empty")]
    Empty,
    #[error("host key file has a type but no key")]
    NoKey,
    #[error(transparent)]
    Type(KeyTypeError),
    #[error(transparent)]
    Key(KeyBase64Error),
}

impl Diagnostic for HostPublicKeyError {
    fn code(&self) -> &'static str {
        match self {
            Self::Empty => "host-public-key.empty",
            Self::NoKey => "host-public-key.no-key",
            Self::Type(_) => "host-public-key.type",
            Self::Key(_) => "host-public-key.key",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::Empty | Self::NoKey => {
                Some("delete the VM's ssh directory so a new key is generated".to_owned())
            }
            Self::Type(inner) => inner.hint(),
            Self::Key(inner) => inner.hint(),
        }
    }
}

impl HostPublicKey {
    /// The first line of a `.pub` file: `TYPE BASE64 [COMMENT]`.
    ///
    /// # Errors
    /// No fields, no key, a type other than ed25519, or a key that is not base64.
    pub fn parse(text: &str) -> Result<Self, HostPublicKeyError> {
        let line = text.lines().next().unwrap_or_default();
        let mut fields = line.split_ascii_whitespace();
        let kind = fields.next().ok_or(HostPublicKeyError::Empty)?;
        let key = fields.next().ok_or(HostPublicKeyError::NoKey)?;
        Ok(Self {
            kind: KeyType::parse(kind).map_err(HostPublicKeyError::Type)?,
            key: KeyBase64::parse(key.to_owned()).map_err(HostPublicKeyError::Key)?,
        })
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::testing::{self, Entry};

    /// A real ed25519 public key blob (68 characters).
    const KEY: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIGq1ZPV0QcVQgRtuRZ8nvRNQdNnwpOAiIUUwwO8M3DJq";

    fn witness(e: &HostPublicKeyError) -> &'static str {
        match e {
            HostPublicKeyError::Empty => "Empty",
            HostPublicKeyError::NoKey => "NoKey",
            HostPublicKeyError::Type(_) => "Type",
            HostPublicKeyError::Key(_) => "Key",
        }
    }

    fn key_witness(e: &KeyBase64Error) -> &'static str {
        match e {
            KeyBase64Error::Empty => "Empty",
            KeyBase64Error::BadChar { .. } => "BadChar",
            KeyBase64Error::TooMuchPadding => "TooMuchPadding",
            KeyBase64Error::BadLength { .. } => "BadLength",
        }
    }

    fn type_witness(e: &KeyTypeError) -> &'static str {
        match e {
            KeyTypeError::Unsupported { .. } => "Unsupported",
        }
    }

    fn type_error() -> KeyTypeError {
        KeyTypeError::Unsupported {
            found: "ssh-rsa".to_owned(),
        }
    }

    fn errors() -> Vec<HostPublicKeyError> {
        vec![
            HostPublicKeyError::Empty,
            HostPublicKeyError::NoKey,
            HostPublicKeyError::Type(type_error()),
            HostPublicKeyError::Key(KeyBase64Error::Empty),
        ]
    }

    fn key_errors() -> Vec<KeyBase64Error> {
        vec![
            KeyBase64Error::Empty,
            KeyBase64Error::BadChar { ch: ' ', at: 3 },
            KeyBase64Error::TooMuchPadding,
            KeyBase64Error::BadLength { len: 5 },
        ]
    }

    /// One catalogue entry per variant of the three enums.
    pub(crate) fn entries() -> Vec<Entry> {
        errors()
            .iter()
            .map(Entry::of)
            .chain(key_errors().iter().map(Entry::of))
            .chain([Entry::of(&type_error())])
            .collect()
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(&errors(), witness, &["Empty", "NoKey", "Type", "Key"]);
        testing::every_variant(
            &key_errors(),
            key_witness,
            &["Empty", "BadChar", "TooMuchPadding", "BadLength"],
        );
        testing::every_variant(&[type_error()], type_witness, &["Unsupported"]);
    }

    #[test]
    fn a_wrapped_error_keeps_the_message_and_hint_of_its_cause() {
        for error in errors() {
            match &error {
                HostPublicKeyError::Type(inner) => {
                    assert_eq!(error.message(), inner.message());
                    assert_eq!(error.hint(), inner.hint());
                }
                HostPublicKeyError::Key(inner) => {
                    assert_eq!(error.message(), inner.message());
                    assert_eq!(error.hint(), inner.hint());
                }
                HostPublicKeyError::Empty | HostPublicKeyError::NoKey => {}
            }
        }
    }

    #[test]
    fn key_types_parse_to_themselves_and_spell_the_same() {
        assert_eq!(KeyType::parse("ssh-ed25519"), Ok(KeyType::Ed25519));
        assert_eq!(KeyType::Ed25519.to_string(), "ssh-ed25519");
    }

    #[test]
    fn other_key_types_are_refused_naming_the_type_cut_at_32_characters() {
        let long = "x".repeat(40);
        testing::table(
            &[
                ("ssh-rsa", "ssh-rsa".to_owned()),
                ("ecdsa-sha2-nistp256", "ecdsa-sha2-nistp256".to_owned()),
                ("", String::new()),
                ("SSH-ED25519", "SSH-ED25519".to_owned()),
                (long.as_str(), "x".repeat(32)),
            ],
            |token| match KeyType::parse(token) {
                Err(KeyTypeError::Unsupported { found }) => found,
                Ok(_) => "accepted".to_owned(),
            },
        );
    }

    #[test]
    fn base64_keys_are_accepted_or_refused_at_every_edge() {
        testing::table(
            &[
                ("", Err(KeyBase64Error::Empty)),
                ("AAAA", Ok(())),
                ("AAA=", Ok(())),
                ("AA==", Ok(())),
                ("A+/9", Ok(())),
                (KEY, Ok(())),
                ("A===", Err(KeyBase64Error::TooMuchPadding)),
                ("====", Err(KeyBase64Error::TooMuchPadding)),
                ("AAAAA", Err(KeyBase64Error::BadLength { len: 5 })),
                ("AAA", Err(KeyBase64Error::BadLength { len: 3 })),
                ("=", Err(KeyBase64Error::BadLength { len: 1 })),
                ("AA=A", Err(KeyBase64Error::BadChar { ch: '=', at: 2 })),
                ("AA A", Err(KeyBase64Error::BadChar { ch: ' ', at: 2 })),
                ("AAA-", Err(KeyBase64Error::BadChar { ch: '-', at: 3 })),
                ("AAA\n", Err(KeyBase64Error::BadChar { ch: '\n', at: 3 })),
                ("AéAA", Err(KeyBase64Error::BadChar { ch: 'é', at: 1 })),
                ("AA==A", Err(KeyBase64Error::BadChar { ch: '=', at: 2 })),
            ],
            |text| KeyBase64::parse((*text).to_owned()).map(|_| ()),
        );
    }

    fn key(text: &str) -> KeyBase64 {
        KeyBase64::parse(text.to_owned()).unwrap()
    }

    #[test]
    fn a_pub_file_is_the_type_and_the_key_whatever_follows() {
        let ed = |text: &str| {
            Ok(HostPublicKey {
                kind: KeyType::Ed25519,
                key: key(text),
            })
        };
        let comment = format!("ssh-ed25519 {KEY} amoeboid-proj\n");
        let bare = format!("ssh-ed25519 {KEY}");
        let spaced = format!("  ssh-ed25519\t{KEY}   a b c\r\nsecond line\n");
        testing::table(
            &[
                (comment.as_str(), ed(KEY)),
                (bare.as_str(), ed(KEY)),
                (spaced.as_str(), ed(KEY)),
                ("", Err(HostPublicKeyError::Empty)),
                ("\n", Err(HostPublicKeyError::Empty)),
                ("   \n", Err(HostPublicKeyError::Empty)),
                ("ssh-ed25519\n", Err(HostPublicKeyError::NoKey)),
                (
                    "ssh-rsa AAAAB3NzaC1yc2E= me\n",
                    Err(HostPublicKeyError::Type(type_error())),
                ),
                (
                    "ssh-ed25519 not/base64!\n",
                    Err(HostPublicKeyError::Key(KeyBase64Error::BadChar {
                        ch: '!',
                        at: 10,
                    })),
                ),
                ("\nssh-ed25519 AAAA\n", Err(HostPublicKeyError::Empty)),
            ],
            |text| HostPublicKey::parse(text),
        );
    }
}
