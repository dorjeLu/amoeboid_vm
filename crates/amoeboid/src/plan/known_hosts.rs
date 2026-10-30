//! The VM's pinned identity: the alias its host key is known under and the `known_hosts` line.

use std::fmt;

use crate::domain::{HostPublicKey, VmName};

/// `amoeboid-NAME`: what the host key is pinned under, whatever port the VM gets (ssh's
/// `HostKeyAlias`), and the comment of the generated key.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct HostAlias(String);

impl HostAlias {
    #[must_use]
    pub fn of(vm: &VmName) -> Self {
        Self(format!("amoeboid-{vm}"))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for HostAlias {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The text of the VM's `known_hosts`: one line, `ALIAS TYPE KEY`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct KnownHosts(String);

impl KnownHosts {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.0.into_bytes()
    }
}

/// Pins `key` under the VM's alias. Both parts are refined, so the line can't be split or
/// extended by what the key file said.
#[must_use]
pub fn known_hosts(vm: &VmName, key: &HostPublicKey) -> KnownHosts {
    KnownHosts(format!("{} {} {}\n", HostAlias::of(vm), key.kind, key.key))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::domain::{KeyBase64, KeyType};
    use crate::testing;

    fn vm(name: &str) -> VmName {
        VmName::parse(name.to_owned()).unwrap()
    }

    #[test]
    fn the_alias_is_the_vm_name_after_amoeboid() {
        testing::table(
            &[
                ("proj", "amoeboid-proj".to_owned()),
                ("A-1", "amoeboid-A-1".to_owned()),
            ],
            |name| HostAlias::of(&vm(name)).to_string(),
        );
        assert_eq!(HostAlias::of(&vm("p")).as_str(), "amoeboid-p");
    }

    #[test]
    fn the_pin_is_one_line_of_alias_type_and_key() {
        let key = HostPublicKey {
            kind: KeyType::Ed25519,
            key: KeyBase64::parse("AAAAC3NzaC1lZDI1NTE5AAAAIAAA".to_owned()).unwrap(),
        };
        let pin = known_hosts(&vm("proj"), &key);
        assert_eq!(
            pin.as_str(),
            "amoeboid-proj ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAA\n"
        );
        assert_eq!(pin.into_bytes(), pin_bytes());
    }

    fn pin_bytes() -> Vec<u8> {
        b"amoeboid-proj ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAA\n".to_vec()
    }
}
