//! `SshBanner`: the identification line sshd sends first (RFC 4253 section 4.2), without its line
//! end. qemu accepts on the forwarded port before the guest is up, so a connection alone proves
//! nothing; a line that is an ssh banner does.

use crate::diagnostic::Diagnostic;
use crate::refined::{Refined, Refinement};

/// The line starts with this.
pub const PREFIX: &str = "SSH-";

/// RFC 4253: at most 255 bytes including the line end.
pub const MAX_LEN: usize = 253;

/// Marker of an ssh identification line.
pub struct Identification;

/// An identification line `SSH-protoversion-softwareversion[ comments]`, printable ASCII, at most
/// 253 characters.
pub type SshBanner = Refined<String, Identification>;

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum SshBannerError {
    #[error("not an ssh banner: it doesn't start with {PREFIX}")]
    NotSsh,
    #[error("not an ssh banner: nothing follows {PREFIX}")]
    NoVersion,
    #[error("not an ssh banner: {len} characters are more than {MAX_LEN}")]
    TooLong { len: usize },
    /// `at` counts characters from the start.
    #[error("not an ssh banner: {ch:?} at position {at} isn't printable ASCII")]
    BadChar { ch: char, at: usize },
}

impl Diagnostic for SshBannerError {
    fn code(&self) -> &'static str {
        match self {
            Self::NotSsh => "ssh-banner.not-ssh",
            Self::NoVersion => "ssh-banner.no-version",
            Self::TooLong { .. } => "ssh-banner.too-long",
            Self::BadChar { .. } => "ssh-banner.bad-char",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some(
            "something other than sshd answers on the forwarded port; see the console log"
                .to_owned(),
        )
    }
}

impl Refinement<String> for Identification {
    type Error = SshBannerError;

    fn check(value: &String) -> Result<(), SshBannerError> {
        let Some(rest) = value.strip_prefix(PREFIX) else {
            return Err(SshBannerError::NotSsh);
        };
        let len = value.chars().count();
        if len > MAX_LEN {
            return Err(SshBannerError::TooLong { len });
        }
        if rest.is_empty() {
            return Err(SshBannerError::NoVersion);
        }
        match value
            .chars()
            .enumerate()
            .find(|&(_, ch)| !(' '..='~').contains(&ch))
        {
            Some((at, ch)) => Err(SshBannerError::BadChar { ch, at }),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::testing::{self, Entry};

    fn witness(e: &SshBannerError) -> &'static str {
        match e {
            SshBannerError::NotSsh => "NotSsh",
            SshBannerError::NoVersion => "NoVersion",
            SshBannerError::TooLong { .. } => "TooLong",
            SshBannerError::BadChar { .. } => "BadChar",
        }
    }

    fn errors() -> Vec<SshBannerError> {
        vec![
            SshBannerError::NotSsh,
            SshBannerError::NoVersion,
            SshBannerError::TooLong { len: 254 },
            SshBannerError::BadChar {
                ch: '\u{1b}',
                at: 9,
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
            &["NotSsh", "NoVersion", "TooLong", "BadChar"],
        );
    }

    #[test]
    fn banners_are_accepted_or_refused_at_every_edge() {
        let longest = format!("SSH-{}", "x".repeat(MAX_LEN - 4));
        let too_long = format!("SSH-{}", "x".repeat(MAX_LEN - 3));
        testing::table(
            &[
                ("SSH-2.0-OpenSSH_9.9", Ok(())),
                ("SSH-2.0-OpenSSH_9.9 Debian-1 ~comments", Ok(())),
                ("SSH-x", Ok(())),
                (longest.as_str(), Ok(())),
                (too_long.as_str(), Err(SshBannerError::TooLong { len: 254 })),
                ("", Err(SshBannerError::NotSsh)),
                ("SSH", Err(SshBannerError::NotSsh)),
                ("ssh-2.0-x", Err(SshBannerError::NotSsh)),
                ("HTTP/1.1 400 Bad Request", Err(SshBannerError::NotSsh)),
                (" SSH-2.0-x", Err(SshBannerError::NotSsh)),
                ("SSH-", Err(SshBannerError::NoVersion)),
                (
                    "SSH-2.0-a\u{1b}[2J",
                    Err(SshBannerError::BadChar {
                        ch: '\u{1b}',
                        at: 9,
                    }),
                ),
                (
                    "SSH-2.0-a\r",
                    Err(SshBannerError::BadChar { ch: '\r', at: 9 }),
                ),
                (
                    "SSH-2.0-\u{7f}",
                    Err(SshBannerError::BadChar {
                        ch: '\u{7f}',
                        at: 8,
                    }),
                ),
                ("SSH-2.0-é", Err(SshBannerError::BadChar { ch: 'é', at: 8 })),
                ("SSH-2.0-~ ", Ok(())),
            ],
            |text| SshBanner::parse((*text).to_owned()).map(|_| ()),
        );
    }
}
