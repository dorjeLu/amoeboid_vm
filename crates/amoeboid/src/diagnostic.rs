//! How errors and warnings present themselves (design §Errors/Diagnostic): a stable code, a
//! message and an optional remedy. Implemented with a full `match` per enum, so a new variant
//! does not compile without its code and hint.

/// Whether a diagnostic stops the operation or only accompanies a successful one.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    /// The word the CLI prints after `amoeboid: `.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
        }
    }
}

/// An error or warning as the operator sees it.
pub trait Diagnostic {
    /// Stable id, dotted lowercase segments, e.g. `start.lock.held`; unique across the library.
    fn code(&self) -> &'static str;

    /// Lowercase, no trailing period, action + object (`can't write {path}: {strerror}`).
    fn message(&self) -> String;

    /// An imperative remedy, when there is one.
    fn hint(&self) -> Option<String>;
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testing::{self, Entry};

    #[derive(Debug)]
    enum Sample {
        Bare,
        Hinted { path: &'static str },
    }

    impl Diagnostic for Sample {
        fn code(&self) -> &'static str {
            match self {
                Self::Bare => "sample.bare",
                Self::Hinted { .. } => "sample.hinted",
            }
        }

        fn message(&self) -> String {
            match self {
                Self::Bare => "can't do it".to_owned(),
                Self::Hinted { path } => format!("can't write {path}: no space left"),
            }
        }

        fn hint(&self) -> Option<String> {
            match self {
                Self::Bare => None,
                Self::Hinted { .. } => Some("free some space".to_owned()),
            }
        }
    }

    #[test]
    fn severity_labels_are_the_words_the_cli_prints() {
        testing::table(
            &[(Severity::Error, "error"), (Severity::Warning, "warning")],
            |s| s.label(),
        );
    }

    #[test]
    fn entry_of_a_diagnostic_carries_code_message_and_hint() {
        testing::table(
            &[
                (
                    Sample::Bare,
                    Entry {
                        code: "sample.bare",
                        message: "can't do it".to_owned(),
                        hint: None,
                    },
                ),
                (
                    Sample::Hinted { path: "/a" },
                    Entry {
                        code: "sample.hinted",
                        message: "can't write /a: no space left".to_owned(),
                        hint: Some("free some space".to_owned()),
                    },
                ),
            ],
            Entry::of,
        );
    }

    /// Every library diagnostic type contributes its table here as it is added (one entry per
    /// variant, taken from the witness table of the type).
    fn sources() -> Vec<Entry> {
        use crate::domain::{
            abs_path, base_spec, contract_version, file_mode, file_name, flake_path, gid,
            guest_home, guest_path, guest_port, guest_rel, home_rel, host_public_key, identity,
            kernel_version, loopback_port, mount, mount_spec, nix_source, observed, pid,
            preset_name, proc_identity, proc_stat, seconds, share_tag_name, shares, socket,
            spec_path, ssh_banner, ssh_port, state_file_name, term_size, timeout, uid, user_name,
            vm_name, word,
        };
        [
            crate::kdl::value_tests::entries(),
            crate::kdl::parse_tests::entries(),
            crate::config::problem_tests::entries(),
            crate::layout::tests::entries(),
            crate::ops::config::tests::entries(),
            crate::ops::home_files::decide_tests::entries(),
            crate::ops::host_key::tests::entries(),
            crate::ops::nix_config::decide_tests::entries(),
            crate::ops::nix_config::tests::entries(),
            crate::ops::records::tests::entries(),
            crate::plan::qemu::tests::entries(),
            crate::plan::qmp::tests::entries(),
            crate::ports::guest_delivery::tests::entries(),
            crate::ports::host_fs::tests::entries(),
            crate::ports::nix::tests::entries(),
            crate::ports::procs::tests::entries(),
            crate::ports::procs::tests::spawn_entries(),
            crate::ports::qmp::tests::entries(),
            crate::ports::session::tests::entries(),
            crate::ports::ssh::tests::entries(),
            crate::ports::system::tests::entries(),
            crate::ops::build::tests::entries(),
            crate::ops::preflight::decide_tests::entries(),
            crate::ops::preflight::tests::entries(),
            crate::ports::vm_lock::tests::entries(),
            crate::records::last_shares_tests::entries(),
            crate::records::run_tests::entries(),
            vm_name::tests::entries(),
            user_name::tests::entries(),
            preset_name::tests::entries(),
            uid::tests::entries(),
            gid::tests::entries(),
            seconds::tests::entries(),
            timeout::tests::entries(),
            ssh_port::tests::entries(),
            guest_port::tests::entries(),
            guest_rel::tests::entries(),
            loopback_port::tests::entries(),
            pid::tests::entries(),
            proc_identity::tests::entries(),
            proc_stat::tests::entries(),
            file_mode::tests::entries(),
            file_name::tests::entries(),
            flake_path::tests::entries(),
            term_size::tests::entries(),
            contract_version::tests::entries(),
            abs_path::tests::entries(),
            guest_path::tests::entries(),
            guest_home::tests::entries(),
            guest_home::tests::observe_entries(),
            home_rel::tests::entries(),
            identity::tests::entries(),
            spec_path::tests::entries(),
            mount_spec::tests::entries(),
            base_spec::tests::entries(),
            host_public_key::tests::entries(),
            nix_source::tests::entries(),
            ssh_banner::tests::entries(),
            mount::tests::entries(),
            observed::tests::entries(),
            shares::tests::entries(),
            socket::tests::entries(),
            kernel_version::tests::entries(),
            crate::contract::tests::entries(),
            share_tag_name::tests::entries(),
            state_file_name::tests::entries(),
            word::tests::entries(),
            crate::layout::tests::socket_entries(),
        ]
        .concat()
    }

    /// Dotted lowercase segments: `start.lock.held`.
    fn code_is_well_formed(code: &str) -> bool {
        !code.is_empty()
            && code.split('.').all(|segment| {
                !segment.is_empty()
                    && segment
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            })
    }

    fn style_problems(entries: &[Entry]) -> Vec<String> {
        let mut problems = Vec::new();
        for entry in entries {
            if !code_is_well_formed(entry.code) {
                problems.push(format!("{}: malformed code", entry.code));
            }
            let texts = std::iter::once(("message", &entry.message))
                .chain(entry.hint.as_ref().map(|hint| ("hint", hint)));
            for (what, text) in texts {
                if text.is_empty() || text.ends_with('.') || text.contains('\n') {
                    problems.push(format!("{}: bad {what}: {text:?}", entry.code));
                }
            }
        }
        problems
    }

    #[test]
    fn code_grammar_accepts_dotted_lowercase_segments_only() {
        testing::table(
            &[
                ("start.lock.held", true),
                ("a", true),
                ("stop.forced-kill", true),
                ("v1.x2", true),
                ("", false),
                (".a", false),
                ("a.", false),
                ("a..b", false),
                ("Start.lock", false),
                ("a b", false),
                ("a_b", false),
            ],
            |code| code_is_well_formed(code),
        );
    }

    #[test]
    fn style_check_flags_trailing_period_empty_text_and_newlines() {
        let entry = |message: &str, hint: Option<&str>| Entry {
            code: "x.y",
            message: message.to_owned(),
            hint: hint.map(str::to_owned),
        };
        testing::table(
            &[
                (vec![entry("can't a", Some("do b"))], 0),
                (vec![entry("can't a.", None)], 1),
                (vec![entry("", None)], 1),
                (vec![entry("can't a", Some("do b."))], 1),
                (vec![entry("a\nb", Some(""))], 2),
                (
                    vec![Entry {
                        code: "Bad",
                        message: "can't a".to_owned(),
                        hint: None,
                    }],
                    1,
                ),
            ],
            |entries| style_problems(entries).len(),
        );
    }

    #[test]
    fn catalogue_of_every_diagnostic_is_written_with_unique_codes() {
        let entries = sources();
        assert_eq!(style_problems(&entries), Vec::<String>::new());
        let text = testing::catalogue(&entries).expect("duplicate diagnostic codes");
        testing::write_golden("diagnostics.txt", &text).unwrap();
    }
}
