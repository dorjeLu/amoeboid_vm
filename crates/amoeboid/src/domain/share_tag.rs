//! `ShareTag`: `d1..d65535`, the virtiofs tag of the Nth shared directory; it also names the
//! share's socket (`dN.sock`) and qemu chardev (`amoN`).

use std::fmt;
use std::num::NonZeroU16;

/// The tag of a shared directory. Every `NonZeroU16` is one, so [`ShareTag::nth`] is total.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ShareTag(NonZeroU16);

impl ShareTag {
    /// The tag of the `index`th shared directory, counting from 1.
    #[must_use]
    pub const fn nth(index: NonZeroU16) -> Self {
        Self(index)
    }

    /// The tag spelled exactly as [`Display`](fmt::Display) writes it: `d`, then the index in
    /// decimal without sign or leading zeros.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let index = text.strip_prefix('d')?.parse::<NonZeroU16>().ok()?;
        let tag = Self::nth(index);
        (tag.to_string() == text).then_some(tag)
    }

    #[must_use]
    pub const fn index(self) -> NonZeroU16 {
        self.0
    }
}

/// `dN`.
impl fmt::Display for ShareTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "d{}", self.0)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testing;

    #[test]
    fn share_tags_parse_only_the_spelling_they_display() {
        testing::table(
            &[
                ("d1", Some(1)),
                ("d12", Some(12)),
                ("d65535", Some(65_535)),
                ("", None),
                ("d", None),
                ("d0", None),
                ("d01", None),
                ("d+1", None),
                ("d-1", None),
                ("D1", None),
                ("d65536", None),
                ("x1", None),
                (" d1", None),
                ("d1 ", None),
                ("d1.sock", None),
            ],
            |text| ShareTag::parse(text).map(|tag| tag.index().get()),
        );
    }

    #[test]
    fn share_tags_are_d_and_their_index() {
        testing::table(
            &[
                (1, "d1".to_owned()),
                (12, "d12".to_owned()),
                (u16::MAX, "d65535".to_owned()),
            ],
            |n| {
                let index = NonZeroU16::new(*n).unwrap();
                let tag = ShareTag::nth(index);
                assert_eq!(tag.index(), index);
                tag.to_string()
            },
        );
    }
}
