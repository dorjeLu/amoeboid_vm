//! The virtiofsd instances of a start and the guest's `mounts` table.

use std::fmt;
use std::num::NonZeroU16;

use crate::domain::{AbsDir, FileName, Mode, ShareTag, Shares, Socket};
use crate::refined;

/// What a virtiofsd instance serves; names its socket and its process.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ShareRole {
    /// `/nix/store`, read-only.
    Store,
    /// The guest-writable state dir, mounted at `/mnt/amoeboid`.
    State,
    /// A shared directory.
    Dir(ShareTag),
}

impl ShareRole {
    /// The role spelled as [`Display`](fmt::Display) writes it.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text.strip_prefix("virtiofsd-")? {
            "ro-store" => Some(Self::Store),
            "state" => Some(Self::State),
            tag => ShareTag::parse(tag).map(Self::Dir),
        }
    }

    #[must_use]
    pub fn socket(self) -> Socket {
        match self {
            Self::Store => Socket::Store,
            Self::State => Socket::State,
            Self::Dir(tag) => Socket::Share(tag),
        }
    }
}

/// `virtiofsd-ro-store`, `virtiofsd-state`, `virtiofsd-dN`.
impl fmt::Display for ShareRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store => f.write_str("virtiofsd-ro-store"),
            Self::State => f.write_str("virtiofsd-state"),
            Self::Dir(tag) => write!(f, "virtiofsd-{tag}"),
        }
    }
}

/// One virtiofsd instance: a host directory served on its role's socket.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Share {
    pub role: ShareRole,
    pub source: AbsDir,
    pub mode: Mode,
}

/// The shares of a start in socket order: the store, the state dir, then one per mount with tags
/// `d1, d2, ...` in mount order. Only [`share_list`] makes one; [`Shares`] guarantees a tag and a
/// path the mounts table can hold for every mount.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ShareList(Vec<Share>);

/// Shares for validated `shares` (parents first), with `guest_state` served as the state share.
/// Total: `Shares` holds at most 65535 mounts, so every one gets a tag.
#[must_use]
pub fn share_list(guest_state: &AbsDir, shares: &Shares) -> ShareList {
    let store = AbsDir::root()
        .join_dir(&refined!(FileName, "nix"))
        .join_dir(&refined!(FileName, "store"));
    let fixed = [
        Share {
            role: ShareRole::Store,
            source: store,
            mode: Mode::Ro,
        },
        Share {
            role: ShareRole::State,
            source: guest_state.clone(),
            mode: Mode::Rw,
        },
    ];
    // `Shares` holds at most `u16::MAX` mounts, so the zip keeps every one.
    let tags = (1..=u16::MAX)
        .filter_map(NonZeroU16::new)
        .map(ShareTag::nth);
    let dirs = shares.iter().zip(tags).map(|(mount, tag)| Share {
        role: ShareRole::Dir(tag),
        source: mount.dir.path().clone(),
        mode: mount.mode,
    });
    ShareList(fixed.into_iter().chain(dirs).collect())
}

impl ShareList {
    #[must_use]
    pub fn as_slice(&self) -> &[Share] {
        &self.0
    }

    pub fn iter(&self) -> impl Iterator<Item = &Share> {
        self.0.iter()
    }

    /// The shared directories with their tags, in mount order.
    pub fn dirs(&self) -> impl Iterator<Item = (ShareTag, &Share)> {
        self.0.iter().filter_map(|share| match share.role {
            ShareRole::Dir(tag) => Some((tag, share)),
            ShareRole::Store | ShareRole::State => None,
        })
    }

    /// The guest's `mounts` file: `<tag>\t<mode>\t<path>\n` per directory, in mount order.
    #[must_use]
    pub fn mounts_table(&self) -> MountsTable {
        let mut table = String::new();
        for (tag, share) in self.dirs() {
            table.extend([
                tag.to_string().as_str(),
                "\t",
                share.mode.as_str(),
                "\t",
                share.source.get(),
                "\n",
            ]);
        }
        MountsTable(table)
    }
}

/// The text of the guest's `mounts` file (state share), read by `base.nix`'s mount service.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MountsTable(String);

impl MountsTable {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::plan::fixtures::{abs, shares, shares_from_golden};
    use crate::testing;

    #[test]
    fn roles_are_named_after_their_socket() {
        let tag = ShareTag::nth(NonZeroU16::new(3).unwrap());
        testing::table(
            &[
                (
                    ShareRole::Store,
                    ("virtiofsd-ro-store".to_owned(), Socket::Store),
                ),
                (
                    ShareRole::State,
                    ("virtiofsd-state".to_owned(), Socket::State),
                ),
                (
                    ShareRole::Dir(tag),
                    ("virtiofsd-d3".to_owned(), Socket::Share(tag)),
                ),
            ],
            |role| (role.to_string(), role.socket()),
        );
    }

    #[test]
    fn roles_parse_only_the_spelling_they_display() {
        let tag = ShareTag::nth(NonZeroU16::new(3).unwrap());
        testing::table(
            &[
                ("virtiofsd-ro-store", Some(ShareRole::Store)),
                ("virtiofsd-state", Some(ShareRole::State)),
                ("virtiofsd-d3", Some(ShareRole::Dir(tag))),
                ("", None),
                ("virtiofsd-", None),
                ("ro-store", None),
                ("virtiofsd-d0", None),
                ("virtiofsd-d03", None),
                ("virtiofsd-store", None),
                ("virtiofsd-d3 ", None),
            ],
            |text| ShareRole::parse(text),
        );
    }

    #[test]
    fn the_store_and_state_come_first_then_the_mounts_with_tags_in_order() {
        let tag = |n| ShareTag::nth(NonZeroU16::new(n).unwrap());
        let mounts = [
            ("/srv/a", Mode::Rw),
            ("/srv/a/b", Mode::Ro),
            ("/srv/c", Mode::Rw),
        ];
        let share = |role, source: &str, mode| Share {
            role,
            source: abs(source),
            mode,
        };
        testing::table(
            &[
                (0, vec![]),
                (1, vec![share(ShareRole::Dir(tag(1)), "/srv/a", Mode::Rw)]),
                (
                    3,
                    vec![
                        share(ShareRole::Dir(tag(1)), "/srv/a", Mode::Rw),
                        share(ShareRole::Dir(tag(2)), "/srv/a/b", Mode::Ro),
                        share(ShareRole::Dir(tag(3)), "/srv/c", Mode::Rw),
                    ],
                ),
            ],
            |count| {
                let list = share_list(&abs("/g"), &shares(mounts.get(..*count).unwrap()));
                let (fixed, dirs) = list.as_slice().split_at(2);
                assert_eq!(
                    fixed,
                    [
                        share(ShareRole::Store, "/nix/store", Mode::Ro),
                        share(ShareRole::State, "/g", Mode::Rw),
                    ]
                );
                assert_eq!(list.iter().count(), count + 2);
                dirs.to_vec()
            },
        );
    }

    #[test]
    fn mounts_given_out_of_order_are_tagged_parents_first() {
        let list = share_list(
            &abs("/g"),
            &shares(&[("/srv/a/b", Mode::Ro), ("/srv/a", Mode::Rw)]),
        );
        let dirs: Vec<(String, String, Mode)> = list
            .dirs()
            .map(|(tag, share)| (tag.to_string(), share.source.to_string(), share.mode))
            .collect();
        assert_eq!(
            dirs,
            [
                ("d1".to_owned(), "/srv/a".to_owned(), Mode::Rw),
                ("d2".to_owned(), "/srv/a/b".to_owned(), Mode::Ro),
            ]
        );
    }

    #[test]
    fn the_most_mounts_that_shares_holds_are_all_tagged() {
        let paths: Vec<String> = (0..crate::domain::shares::MAX_SHARES)
            .map(|n| format!("/m/d{n}"))
            .collect();
        let mounts: Vec<(&str, Mode)> = paths.iter().map(|p| (p.as_str(), Mode::Rw)).collect();
        let list = share_list(&abs("/g"), &shares(&mounts));
        assert_eq!(list.dirs().count(), 65_535);
        assert_eq!(
            list.dirs().last().map(|(tag, _)| tag.to_string()),
            Some("d65535".to_owned())
        );
    }

    #[test]
    fn mounts_tables_hold_one_tab_separated_line_per_mount() {
        let list = share_list(
            &abs("/g"),
            &shares(&[("/srv/a b", Mode::Rw), ("/srv/c\rd", Mode::Ro)]),
        );
        assert_eq!(
            list.mounts_table().as_str(),
            "d1\trw\t/srv/a b\nd2\tro\t/srv/c\rd\n"
        );
    }

    #[test]
    fn mounts_tables_match_the_python_ones() {
        for case in testing::golden_cases("mounts_table", "json", "tsv").unwrap() {
            let input: serde_json::Value = serde_json::from_str(&case.input).unwrap();
            let mounts = shares_from_golden(input.get("mounts").unwrap());
            assert_eq!(
                share_list(&abs("/s/guest"), &mounts)
                    .mounts_table()
                    .as_str(),
                case.output,
                "{}",
                case.name
            );
        }
    }

    #[test]
    fn shares_match_the_python_ones() {
        let cases: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../../tests/golden/shares.json")).unwrap();
        assert!(cases.len() > 1);
        for case in cases {
            let input = case.get("input").unwrap();
            let text = |v: &serde_json::Value, key: &str| {
                v.get(key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap()
                    .to_owned()
            };
            let mounts = shares_from_golden(input.get("mounts").unwrap());
            let list = share_list(&abs(&text(input, "guest_state")), &mounts);
            let got: Vec<serde_json::Value> = list
                .iter()
                .map(|share| {
                    serde_json::json!({
                        "role": share.role.to_string(),
                        "socket": share.role.socket().name().as_str(),
                        "source": share.source.get(),
                        "mode": share.mode.as_str(),
                    })
                })
                .collect();
            let expected = case.get("output").and_then(|o| o.get("shares")).unwrap();
            assert_eq!(
                &serde_json::Value::Array(got),
                expected,
                "{}",
                text(&case, "name")
            );
        }
    }
}
