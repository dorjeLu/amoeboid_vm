//! `GuestDest`: where in the guest-visible share (`<data>/NAME/guest`, at `/mnt/amoeboid`) a file
//! the host puts there belongs (design §Home storage). With home storage `share` the guest's home
//! is the share's `home/` directory; the `volume` layout is not implemented, so `rel` is the
//! `share` layout.

use crate::domain::file_mode::FileMode;
use crate::domain::file_name::FileName;
use crate::domain::guest_rel::{GuestFile, GuestRel};
use crate::domain::home_rel::HomeRel;
use crate::domain::state_file_name::StateFileName;
use crate::refined;

const HOME: FileName = refined!(FileName, "home");
const AMOEBOID: FileName = refined!(FileName, "amoeboid");
const NIX: FileName = refined!(FileName, "nix");
const REFERENCE: FileName = refined!(FileName, "reference");
const README: FileName = refined!(FileName, "README.md");

/// What a file is for, which says where it goes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum GuestDest {
    /// A file the guest reads at boot (`mounts`, `hostname`, `term`, the host key).
    State(StateFileName),
    /// A file of the host user's home, at the same place in the guest's home.
    Home(HomeRel),
    /// A file of the VM's own, editable copy of its configuration.
    OwnConfig(GuestRel),
    /// A file of the configuration the VM is running.
    Reference(GuestRel),
    Readme,
}

impl GuestDest {
    /// The guest's home in the share.
    #[must_use]
    pub fn home_dir() -> GuestRel {
        GuestRel::of(&HOME)
    }

    fn amoeboid_dir() -> GuestRel {
        Self::home_dir().join(&GuestRel::of(&AMOEBOID))
    }

    /// The directory of the VM's own copy of its configuration.
    #[must_use]
    pub fn own_dir() -> GuestRel {
        Self::amoeboid_dir().join(&GuestRel::of(&NIX))
    }

    /// The directory of the configuration the VM is running.
    #[must_use]
    pub fn reference_dir() -> GuestRel {
        Self::amoeboid_dir().join(&GuestRel::of(&REFERENCE))
    }

    /// The path in the share.
    #[must_use]
    pub fn rel(&self) -> GuestRel {
        match self {
            Self::State(name) => GuestRel::of(name),
            Self::Home(rel) => Self::home_dir().join(&GuestRel::of_home(rel)),
            Self::OwnConfig(rel) => Self::own_dir().join(rel),
            Self::Reference(rel) => Self::reference_dir().join(rel),
            Self::Readme => Self::amoeboid_dir().join(&GuestRel::of(&README)),
        }
    }

    /// A file for this destination.
    #[must_use]
    pub fn file(&self, bytes: Vec<u8>, mode: FileMode) -> GuestFile {
        GuestFile {
            dest: self.rel(),
            bytes,
            mode,
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testing;

    fn rel(text: &str) -> GuestRel {
        GuestRel::parse(text.to_owned()).unwrap()
    }

    #[test]
    fn every_kind_of_file_has_its_place_in_the_share() {
        testing::table(
            &[
                (
                    GuestDest::State(StateFileName::parse("hostname".to_owned()).unwrap()),
                    "hostname".to_owned(),
                ),
                (
                    GuestDest::Home(HomeRel::parse(".config/git/config".to_owned()).unwrap()),
                    "home/.config/git/config".to_owned(),
                ),
                (
                    GuestDest::OwnConfig(rel("sub/base.nix")),
                    "home/amoeboid/nix/sub/base.nix".to_owned(),
                ),
                (
                    GuestDest::Reference(rel("flake.nix")),
                    "home/amoeboid/reference/flake.nix".to_owned(),
                ),
                (GuestDest::Readme, "home/amoeboid/README.md".to_owned()),
            ],
            |dest| dest.rel().to_string(),
        );
    }

    #[test]
    fn the_tree_directories_are_the_parents_of_their_files() {
        assert_eq!(GuestDest::home_dir().to_string(), "home");
        assert_eq!(GuestDest::own_dir().to_string(), "home/amoeboid/nix");
        assert_eq!(
            GuestDest::reference_dir().to_string(),
            "home/amoeboid/reference"
        );
    }

    #[test]
    fn a_file_for_a_destination_is_put_at_its_place() {
        let mode = FileMode::parse(0o600).unwrap();
        assert_eq!(
            GuestDest::Readme.file(b"x".to_vec(), mode),
            GuestFile {
                dest: rel("home/amoeboid/README.md"),
                bytes: b"x".to_vec(),
                mode,
            }
        );
    }
}
