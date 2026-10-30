//! `KernelVersion`: `major.minor` of the running kernel, as the leading numbers of
//! `/proc/sys/kernel/osrelease` (`7.3.0-4-cachyos`, `6.1.0`, `5.15`).

use std::fmt;

use crate::diagnostic::Diagnostic;

/// The first kernel with `openat2` (`RESOLVE_BENEATH`, `RESOLVE_NO_SYMLINKS`) and, with it,
/// everything else the library asks of the kernel (`pidfd_send_signal` came in 5.1).
pub const OPENAT2: KernelVersion = KernelVersion { major: 5, minor: 6 };

/// A kernel release reduced to the part that orders releases. Fields are ordered so the derived
/// `Ord` compares major first.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct KernelVersion {
    major: u32,
    minor: u32,
}

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum KernelVersionError {
    #[error("bad kernel release: it doesn't start with a number")]
    NoMajor,
    #[error("bad kernel release: the major number isn't followed by '.' and a minor number")]
    NoMinor,
}

impl Diagnostic for KernelVersionError {
    fn code(&self) -> &'static str {
        match self {
            Self::NoMajor => "kernel-version.no-major",
            Self::NoMinor => "kernel-version.no-minor",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some("check that /proc is mounted and the kernel release looks like 6.1.0".to_owned())
    }
}

impl KernelVersion {
    /// The leading `MAJOR.MINOR` of a release; whatever follows is ignored.
    ///
    /// # Errors
    /// The release doesn't start with `NUMBER.NUMBER`.
    pub fn parse(release: &str) -> Result<Self, KernelVersionError> {
        let (major, rest) = number(release).ok_or(KernelVersionError::NoMajor)?;
        let minor = rest
            .strip_prefix('.')
            .and_then(number)
            .ok_or(KernelVersionError::NoMinor)?
            .0;
        Ok(Self { major, minor })
    }
}

/// A run of ASCII digits at the start of `text`, and what follows. A value beyond `u32` is
/// not a number.
fn number(text: &str) -> Option<(u32, &str)> {
    let end = text
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(text.len());
    let (digits, rest) = text.split_at(end);
    Some((digits.parse().ok()?, rest))
}

impl fmt::Display for KernelVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::testing::{self, Entry};

    fn witness(e: &KernelVersionError) -> &'static str {
        match e {
            KernelVersionError::NoMajor => "NoMajor",
            KernelVersionError::NoMinor => "NoMinor",
        }
    }

    fn errors() -> Vec<KernelVersionError> {
        vec![KernelVersionError::NoMajor, KernelVersionError::NoMinor]
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        errors().iter().map(Entry::of).collect()
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(&errors(), witness, &["NoMajor", "NoMinor"]);
    }

    #[allow(clippy::unnecessary_wraps, reason = "the table compares whole results")]
    fn version(major: u32, minor: u32) -> Result<String, KernelVersionError> {
        Ok(format!("{major}.{minor}"))
    }

    #[test]
    fn kernel_version_parse_reads_the_leading_major_and_minor() {
        testing::table(
            &[
                ("7.3.0-4-cachyos", version(7, 3)),
                ("6.1.0", version(6, 1)),
                ("5.15", version(5, 15)),
                ("5.6.0-rc1", version(5, 6)),
                ("4.4294967295", version(4, 4_294_967_295)),
                ("", Err(KernelVersionError::NoMajor)),
                ("x.1", Err(KernelVersionError::NoMajor)),
                (" 6.1", Err(KernelVersionError::NoMajor)),
                ("4294967296.1", Err(KernelVersionError::NoMajor)),
                ("6", Err(KernelVersionError::NoMinor)),
                ("6.", Err(KernelVersionError::NoMinor)),
                ("6.x", Err(KernelVersionError::NoMinor)),
                ("6-rc1", Err(KernelVersionError::NoMinor)),
                ("6.4294967296", Err(KernelVersionError::NoMinor)),
            ],
            |release| KernelVersion::parse(release).map(|v| v.to_string()),
        );
    }

    #[test]
    fn versions_order_by_major_then_minor_and_openat2_is_5_6() {
        let v = |text| KernelVersion::parse(text).unwrap();
        assert!(v("5.5") < OPENAT2);
        assert!(v("4.20") < OPENAT2);
        assert!(v("5.6") >= OPENAT2);
        assert!(v("5.10") >= OPENAT2);
        assert!(v("6.0") >= OPENAT2);
        assert_eq!(OPENAT2.to_string(), "5.6");
    }
}
