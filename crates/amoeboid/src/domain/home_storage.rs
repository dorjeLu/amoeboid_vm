//! `HomeStorage`: where the guest's home lives (config key `home-storage`, design §Home storage).

use std::fmt;

/// `Share` is a host directory shared into the guest (implemented). `Volume` is a block-device
/// image; it is modelled and accepted in the config, and `start` reports it as not implemented.
// TODO(volume-home): image path + size; read-only state share; in-guest copy at boot.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum HomeStorage {
    #[default]
    Share,
    Volume,
}

impl HomeStorage {
    /// Every spelling [`HomeStorage::parse`] accepts, in declaration order.
    pub const NAMES: &'static [&'static str] = &["share", "volume"];

    /// The spelling used in `run_config.kdl` and in the build contract.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Share => "share",
            Self::Volume => "volume",
        }
    }

    /// The storage spelled exactly as in [`HomeStorage::NAMES`].
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "share" => Some(Self::Share),
            "volume" => Some(Self::Volume),
            _ => None,
        }
    }
}

impl fmt::Display for HomeStorage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testing;

    #[test]
    fn storages_are_spelled_share_and_volume_and_nothing_else_parses() {
        testing::table(
            &[
                ("share", Some(HomeStorage::Share)),
                ("volume", Some(HomeStorage::Volume)),
                ("", None),
                ("Share", None),
                ("volume ", None),
                ("disk", None),
            ],
            |text| HomeStorage::parse(text),
        );
    }

    #[test]
    fn names_list_every_storage_in_order_and_parse_back_to_it() {
        let all = [HomeStorage::Share, HomeStorage::Volume];
        let names: Vec<&str> = all.iter().map(|storage| storage.as_str()).collect();
        assert_eq!(names, HomeStorage::NAMES);
        for storage in all {
            assert_eq!(storage.to_string(), storage.as_str());
            assert_eq!(HomeStorage::parse(&storage.to_string()), Some(storage));
        }
    }

    #[test]
    fn the_default_storage_is_a_share() {
        assert_eq!(HomeStorage::default(), HomeStorage::Share);
    }
}
