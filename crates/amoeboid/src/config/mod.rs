//! The run configuration: `run_config.kdl` <-> [`RunConfig`] (design §Workspace `config/`).
//!
//! Pure: text in, refined values or every problem out. Reading and writing the file (atomic
//! save, first-run creation) belong to the ports that own the filesystem.

mod decode;
mod encode;
mod problem;
mod run_config;

pub use decode::decode;
pub use encode::{encode, render};
pub use problem::{Block, ConfigError, Key, Location, Problem, ProblemKind, Segment, ValueKind};
pub use run_config::{
    Anchor, DEFAULT_PRESET, HEADER, RunConfig, SshConfig, TimeoutsConfig, default_base,
};

#[cfg(test)]
pub(crate) use problem::tests as problem_tests;

use crate::kdl;

/// Parse the text of a `run_config.kdl`. Relative text is anchored at `anchor`.
///
/// # Errors
///
/// Not KDL (or not the subset amoeboid reads), or not a valid configuration (every problem).
pub fn parse(text: &str, anchor: &Anchor) -> Result<RunConfig, ConfigError> {
    kdl::parse(text)
        .map_err(ConfigError::Syntax)
        .and_then(|doc| decode(&doc, anchor).map_err(ConfigError::Invalid))
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use proptest::collection::vec;
    use proptest::prelude::*;

    use super::*;
    use crate::domain::{
        AbsDir, BaseSpec, BaseSpecError, FixedPortError, Gid, HomeDir, HomeRel, HomeRelError,
        HomeStorage, LingerPolicy, Mode, MountRequest, PresetName, ResolveError, Seconds,
        SecondsError, SpecPathError, SshPort, Uid, UidError, UserName, UserNameError,
    };
    use crate::kdl::Value;
    use crate::rails::NonEmpty;
    use crate::testing;

    fn abs(text: &str) -> AbsDir {
        AbsDir::parse(text.to_owned()).unwrap()
    }

    fn anchor() -> Anchor {
        Anchor {
            config_dir: abs("/cfg"),
            home: HomeDir::parse("/home/alice".to_owned()).unwrap(),
        }
    }

    fn user(text: &str) -> UserName {
        UserName::parse(text.to_owned()).unwrap()
    }

    fn preset(text: &str) -> BaseSpec {
        BaseSpec::Preset(PresetName::parse(text.to_owned()).unwrap())
    }

    fn rel(text: &str) -> HomeRel {
        HomeRel::parse(text.to_owned()).unwrap()
    }

    fn secs(n: u32) -> Seconds {
        Seconds::parse(n).unwrap()
    }

    fn request(path: &str, mode: Mode) -> MountRequest {
        MountRequest {
            path: abs(path),
            mode,
        }
    }

    fn minimal(name: &str, id: u32) -> RunConfig {
        RunConfig::new(user(name), Uid::parse(id).unwrap(), Gid::parse(id).unwrap())
    }

    // -- the rendered example ---------------------------------------------------------------

    fn alice() -> RunConfig {
        RunConfig {
            default_mounts: vec![
                request("/home/alice/dev", Mode::Rw),
                request("/srv", Mode::Ro),
            ],
            home_files: vec![rel(".gitconfig")],
            gid: Gid::parse(100).unwrap(),
            ..minimal("alice", 1000)
        }
    }

    const ALICE_KDL: &str = "\
user \"alice\"
uid 1000
gid 100
base-nix-config \"default\"
(list)default-mounts {
    - {
        path \"/home/alice/dev\"
        mode \"rw\"
    }
    - {
        path \"/srv\"
        mode \"ro\"
    }
}
(list)home-files {
    - \".gitconfig\"
}
ssh {
    port 0
}
timeouts {
    boot 180
    stop 60
}
linger \"enable\"
home-storage \"share\"
";

    #[test]
    fn rendered_example_has_the_header_every_key_and_the_new_keys_last() {
        assert_eq!(render(&alice()), format!("{HEADER}\n{ALICE_KDL}"));
    }

    #[test]
    fn the_example_text_decodes_to_the_example() {
        assert_eq!(parse(ALICE_KDL, &anchor()), Ok(alice()));
    }

    #[test]
    fn every_non_default_setting_is_rendered_and_read_back() {
        let config = RunConfig {
            base_nix_config: BaseSpec::Dir(abs("/opt/my presets/base")),
            ssh: SshConfig {
                port: SshPort::parse(2222).unwrap(),
            },
            timeouts: TimeoutsConfig {
                boot: secs(600),
                stop: secs(5),
            },
            linger: LingerPolicy::Warn,
            home_storage: HomeStorage::Volume,
            ..minimal("bob", 1001)
        };
        let text = render(&config);
        for line in [
            "base-nix-config \"/opt/my presets/base\"\n",
            "    port 2222\n",
            "    boot 600\n",
            "    stop 5\n",
            "linger \"warn\"\n",
            "home-storage \"volume\"\n",
        ] {
            assert!(text.contains(line), "{line:?} in {text}");
        }
        assert_eq!(parse(&text, &anchor()), Ok(config));
    }

    // -- the golden corpus -------------------------------------------------------------------

    fn json_str<'a>(json: &'a serde_json::Value, key: &str) -> &'a str {
        json.get(key).and_then(serde_json::Value::as_str).unwrap()
    }

    fn json_u32(json: &serde_json::Value, key: &str) -> u32 {
        u32::try_from(json.get(key).and_then(serde_json::Value::as_u64).unwrap()).unwrap()
    }

    /// A configuration from the Python generator's field dump.
    fn config_from_golden(json: &serde_json::Value) -> RunConfig {
        let a = anchor();
        let mounts = json
            .get("default_mounts")
            .and_then(|m| m.as_array())
            .unwrap();
        let files = json.get("home_files").and_then(|f| f.as_array()).unwrap();
        let timeouts = json.get("timeouts").unwrap();
        RunConfig {
            user: user(json_str(json, "user")),
            uid: Uid::parse(json_u32(json, "uid")).unwrap(),
            gid: Gid::parse(json_u32(json, "gid")).unwrap(),
            base_nix_config: BaseSpec::parse(
                json_str(json, "base_nix_config"),
                &a.config_dir,
                &a.home,
            )
            .unwrap(),
            default_mounts: mounts
                .iter()
                .map(|m| MountRequest {
                    path: abs(json_str(m, "path")),
                    mode: Mode::from_suffix(json_str(m, "mode")).unwrap(),
                })
                .collect(),
            home_files: files.iter().map(|f| rel(f.as_str().unwrap())).collect(),
            ssh: SshConfig {
                port: SshPort::parse(u16::try_from(json_u32(json, "ssh_port")).unwrap()).unwrap(),
            },
            timeouts: TimeoutsConfig {
                boot: secs(json_u32(timeouts, "boot")),
                stop: secs(json_u32(timeouts, "stop")),
            },
            linger: LingerPolicy::default(),
            home_storage: HomeStorage::default(),
        }
    }

    #[test]
    fn python_written_files_load_with_the_new_keys_at_their_defaults() {
        let cases = testing::golden_cases("config", "json", "kdl").unwrap();
        assert!(cases.len() >= 10);
        for case in cases {
            let expected = config_from_golden(&serde_json::from_str(&case.input).unwrap());
            assert_eq!(
                parse(&case.output, &anchor()),
                Ok(expected),
                "{}",
                case.name
            );
        }
    }

    #[test]
    fn rust_renders_the_python_text_plus_the_two_new_keys() {
        for case in testing::golden_cases("config", "json", "kdl").unwrap() {
            let config = config_from_golden(&serde_json::from_str(&case.input).unwrap());
            assert_eq!(
                render(&config),
                format!("{}linger \"enable\"\nhome-storage \"share\"\n", case.output),
                "{}",
                case.name
            );
        }
    }

    // -- defaults and leniency ---------------------------------------------------------------

    #[test]
    fn a_minimal_file_gets_the_defaults() {
        assert_eq!(
            parse("user \"bob\"\nuid 1001\ngid 1001\n", &anchor()),
            Ok(minimal("bob", 1001))
        );
    }

    #[test]
    fn relative_paths_resolve_against_the_config_dir_and_tilde_against_home() {
        let text = "user \"bob\"; uid 1; gid 1; base-nix-config \"./presets/mine\"\n\
                    (list)default-mounts { - { path \"../dev\" }; - { path \"~/x\"; mode \"ro\" }; - { path \"~\" } }";
        let config = parse(text, &anchor()).unwrap();
        assert_eq!(
            config.base_nix_config,
            BaseSpec::Dir(abs("/cfg/presets/mine"))
        );
        assert_eq!(
            config.default_mounts,
            vec![
                request("/dev", Mode::Rw),
                request("/home/alice/x", Mode::Ro),
                request("/home/alice", Mode::Rw),
            ]
        );
    }

    #[test]
    fn home_files_written_with_dots_and_doubled_slashes_load_normalised() {
        let config = parse(
            "user \"bob\"; uid 1; gid 1\n(list)home-files { - \"./a//b/\"; - \".config/git/config\" }",
            &anchor(),
        )
        .unwrap();
        assert_eq!(
            config.home_files,
            vec![rel("a/b"), rel(".config/git/config")]
        );
    }

    #[test]
    fn timeouts_and_ssh_may_be_given_in_part() {
        let config = parse(
            "user \"bob\"; uid 1; gid 1; timeouts { boot 600 }; ssh { }",
            &anchor(),
        )
        .unwrap();
        assert_eq!(
            config.timeouts,
            TimeoutsConfig {
                boot: secs(600),
                stop: secs(60)
            }
        );
        assert_eq!(config.ssh, SshConfig::DEFAULT);
    }

    // -- errors ------------------------------------------------------------------------------

    fn key(k: Key) -> Segment {
        Segment::Key(k)
    }

    fn problem(path: &[Segment], kind: ProblemKind) -> Problem {
        Problem {
            at: Location::from_segments(path.to_vec()),
            kind,
        }
    }

    fn invalid(problems: Vec<Problem>) -> ConfigError {
        ConfigError::Invalid(NonEmpty::from_vec(problems).unwrap())
    }

    fn not_string(found: ValueKind) -> ProblemKind {
        ProblemKind::NotString { found }
    }

    fn not_int(found: ValueKind) -> ProblemKind {
        ProblemKind::NotInteger { found }
    }

    fn unknown(block: Block, name: &str) -> ProblemKind {
        ProblemKind::UnknownKey {
            block,
            key: name.to_owned(),
        }
    }

    const BOB: &str = "user \"bob\"; uid 1; gid 1\n";

    fn with_bob(extra: &str) -> String {
        format!("{BOB}{extra}")
    }

    #[test]
    fn every_problem_is_reported_at_once_in_document_order() {
        let text = "
            user \"root\"
            uid #true
            base-nix-config 3
            (list)default-mounts { - { path \"/x\"; mode \"rx\" }; - \"/y\" }
            (list)home-files { - \"../escape\"; - \"/abs\" }
            ssh { port 22; colour \"red\" }
            timeouts { boot 0; stop 1.5; kill 3 }
            linger \"never\"
            home-storage 7
            typo 1
        ";
        let mount_mode = [key(Key::DefaultMounts), Segment::Item(0), key(Key::Mode)];
        assert_eq!(
            parse(text, &anchor()),
            Err(invalid(vec![
                problem(&[key(Key::User)], ProblemKind::User(UserNameError::Root)),
                problem(&[key(Key::Uid)], not_int(ValueKind::Bool)),
                problem(&[key(Key::Gid)], ProblemKind::Missing),
                problem(&[key(Key::BaseNixConfig)], not_string(ValueKind::Int)),
                problem(
                    &mount_mode,
                    ProblemKind::BadChoice {
                        found: "rx".to_owned(),
                        allowed: Mode::NAMES
                    }
                ),
                problem(
                    &[key(Key::DefaultMounts), Segment::Item(1)],
                    ProblemKind::NotBlock {
                        block: Block::Mount,
                        found: ValueKind::Str
                    }
                ),
                problem(
                    &[key(Key::HomeFiles), Segment::Item(0)],
                    ProblemKind::HomeFile(HomeRelError::ParentDir { index: 0 })
                ),
                problem(
                    &[key(Key::HomeFiles), Segment::Item(1)],
                    ProblemKind::HomeFile(HomeRelError::Absolute)
                ),
                problem(
                    &[key(Key::Ssh), key(Key::Port)],
                    ProblemKind::Port(FixedPortError::Privileged { value: 22 })
                ),
                problem(&[key(Key::Ssh)], unknown(Block::Ssh, "colour")),
                problem(
                    &[key(Key::Timeouts), key(Key::Boot)],
                    ProblemKind::Seconds(SecondsError::Zero)
                ),
                problem(
                    &[key(Key::Timeouts), key(Key::Stop)],
                    not_int(ValueKind::Float)
                ),
                problem(&[key(Key::Timeouts)], unknown(Block::Timeouts, "kill")),
                problem(
                    &[key(Key::Linger)],
                    ProblemKind::BadChoice {
                        found: "never".to_owned(),
                        allowed: LingerPolicy::NAMES
                    }
                ),
                problem(&[key(Key::HomeStorage)], not_string(ValueKind::Int)),
                problem(&[], unknown(Block::Root, "typo")),
            ]))
        );
    }

    #[test]
    fn the_message_of_a_broken_file_lists_every_problem() {
        let error = parse("user \"root\"\nuid 0\ntypo 1\n", &anchor()).unwrap_err();
        assert_eq!(
            crate::diagnostic::Diagnostic::message(&error),
            "invalid configuration: user: bad account name: root can't be used in the guest; \
             uid: bad user id: 0 belongs to root; gid: missing; unknown key \"typo\"; known: \
             base-nix-config, default-mounts, gid, home-files, home-storage, linger, ssh, \
             timeouts, uid, user"
        );
    }

    /// Each text has exactly the one problem beside it.
    fn rejects(cases: Vec<(String, Problem)>) {
        for (n, (text, expected)) in cases.into_iter().enumerate() {
            assert_eq!(
                parse(&text, &anchor()),
                Err(invalid(vec![expected])),
                "case {n}: {text}"
            );
        }
    }

    #[test]
    fn identity_fields_are_rejected_with_the_error_of_their_type() {
        let uid = [key(Key::Uid)];
        let gid = [key(Key::Gid)];
        rejects(vec![
            (
                "user \"\"; uid 1; gid 1".to_owned(),
                problem(&[key(Key::User)], ProblemKind::User(UserNameError::Empty)),
            ),
            (
                "user 5; uid 1; gid 1".to_owned(),
                problem(&[key(Key::User)], not_string(ValueKind::Int)),
            ),
            (
                "user \"bob\"; uid 0; gid 1".to_owned(),
                problem(&uid, ProblemKind::Uid(UidError::Root)),
            ),
            (
                "user \"bob\"; uid 4294967295; gid 1".to_owned(),
                problem(&uid, ProblemKind::Uid(UidError::Unset)),
            ),
            (
                "user \"bob\"; uid 4294967296; gid 1".to_owned(),
                problem(
                    &uid,
                    ProblemKind::OutOfRange {
                        value: 4_294_967_296,
                        max: u32::MAX,
                    },
                ),
            ),
            (
                "user \"bob\"; uid -1; gid 1".to_owned(),
                problem(
                    &uid,
                    ProblemKind::OutOfRange {
                        value: -1,
                        max: u32::MAX,
                    },
                ),
            ),
            (
                "user \"bob\"; uid \"1\"; gid 1".to_owned(),
                problem(&uid, not_int(ValueKind::Str)),
            ),
            (
                "user \"bob\"; uid #null; gid 1".to_owned(),
                problem(&uid, not_int(ValueKind::Null)),
            ),
            (
                "user \"bob\"; uid 1; gid 0".to_owned(),
                problem(&gid, ProblemKind::Gid(crate::domain::GidError::Root)),
            ),
            (
                "user \"bob\"; uid 1; gid 4294967295".to_owned(),
                problem(&gid, ProblemKind::Gid(crate::domain::GidError::Unset)),
            ),
        ]);
    }

    #[test]
    fn base_mounts_and_home_files_are_rejected_with_the_error_of_their_type() {
        let mount_path = [key(Key::DefaultMounts), Segment::Item(0), key(Key::Path)];
        rejects(vec![
            (
                with_bob("base-nix-config \"\""),
                problem(
                    &[key(Key::BaseNixConfig)],
                    ProblemKind::Base(BaseSpecError::Preset(crate::domain::PresetNameError::Empty)),
                ),
            ),
            (
                with_bob("base-nix-config \"~eve/x\""),
                problem(
                    &[key(Key::BaseNixConfig)],
                    ProblemKind::Base(BaseSpecError::Resolve(ResolveError::OtherUser {
                        name: "eve".to_owned(),
                    })),
                ),
            ),
            (
                with_bob("(list)default-mounts { - { path \"\" } }"),
                problem(&mount_path, ProblemKind::MountPath(SpecPathError::Empty)),
            ),
            (
                with_bob("(list)default-mounts { - { path \"/a\\u{0}b\" } }"),
                problem(
                    &mount_path,
                    ProblemKind::MountPath(SpecPathError::NulByte { at: 2 }),
                ),
            ),
            (
                with_bob("(list)default-mounts { - { path \"~eve/x\" } }"),
                problem(
                    &mount_path,
                    ProblemKind::MountResolve(ResolveError::OtherUser {
                        name: "eve".to_owned(),
                    }),
                ),
            ),
            (
                with_bob("(list)default-mounts { - { mode \"ro\" } }"),
                problem(&mount_path, ProblemKind::Missing),
            ),
            (
                with_bob("(list)default-mounts { - { path \"/a\"; mode 1 } }"),
                problem(
                    &[key(Key::DefaultMounts), Segment::Item(0), key(Key::Mode)],
                    not_string(ValueKind::Int),
                ),
            ),
            (
                with_bob("(list)default-mounts { - { path \"/a\"; colour \"red\" } }"),
                problem(
                    &[key(Key::DefaultMounts), Segment::Item(0)],
                    unknown(Block::Mount, "colour"),
                ),
            ),
            (
                with_bob("default-mounts \"/a\""),
                problem(
                    &[key(Key::DefaultMounts)],
                    ProblemKind::NotList {
                        found: ValueKind::Str,
                    },
                ),
            ),
            (
                with_bob("home-files 3"),
                problem(
                    &[key(Key::HomeFiles)],
                    ProblemKind::NotList {
                        found: ValueKind::Int,
                    },
                ),
            ),
            (
                with_bob("(list)home-files { - \"\" }"),
                problem(
                    &[key(Key::HomeFiles), Segment::Item(0)],
                    ProblemKind::HomeFile(HomeRelError::Empty),
                ),
            ),
            (
                with_bob("(list)home-files { - \"~/x\" }"),
                problem(
                    &[key(Key::HomeFiles), Segment::Item(0)],
                    ProblemKind::HomeFile(HomeRelError::Tilde),
                ),
            ),
            (
                with_bob("(list)home-files { - 4 }"),
                problem(
                    &[key(Key::HomeFiles), Segment::Item(0)],
                    not_string(ValueKind::Int),
                ),
            ),
        ]);
    }

    #[test]
    fn blocks_and_choices_are_rejected_with_the_error_of_their_type() {
        rejects(vec![
            (
                with_bob("ssh 22"),
                problem(
                    &[key(Key::Ssh)],
                    ProblemKind::NotBlock {
                        block: Block::Ssh,
                        found: ValueKind::Int,
                    },
                ),
            ),
            (
                with_bob("ssh { port 70000 }"),
                problem(
                    &[key(Key::Ssh), key(Key::Port)],
                    ProblemKind::OutOfRange {
                        value: 70_000,
                        max: 65_535,
                    },
                ),
            ),
            (
                with_bob("ssh { port \"22\" }"),
                problem(&[key(Key::Ssh), key(Key::Port)], not_int(ValueKind::Str)),
            ),
            (
                with_bob("timeouts #true"),
                problem(
                    &[key(Key::Timeouts)],
                    ProblemKind::NotBlock {
                        block: Block::Timeouts,
                        found: ValueKind::Bool,
                    },
                ),
            ),
            (
                with_bob("timeouts { stop 86401 }"),
                problem(
                    &[key(Key::Timeouts), key(Key::Stop)],
                    ProblemKind::Seconds(SecondsError::TooLarge { value: 86_401 }),
                ),
            ),
            (
                with_bob("linger #true"),
                problem(&[key(Key::Linger)], not_string(ValueKind::Bool)),
            ),
            (
                with_bob("home-storage \"disk\""),
                problem(
                    &[key(Key::HomeStorage)],
                    ProblemKind::BadChoice {
                        found: "disk".to_owned(),
                        allowed: HomeStorage::NAMES,
                    },
                ),
            ),
        ]);
    }

    #[test]
    fn a_missing_required_key_is_reported_at_the_key() {
        assert_eq!(
            parse("", &anchor()),
            Err(invalid(vec![
                problem(&[key(Key::User)], ProblemKind::Missing),
                problem(&[key(Key::Uid)], ProblemKind::Missing),
                problem(&[key(Key::Gid)], ProblemKind::Missing),
            ]))
        );
    }

    #[test]
    fn text_that_is_not_the_kdl_subset_is_a_syntax_error() {
        for text in ["user {", "user \"a\" \"b\"", "a=1"] {
            let parsed = parse(text, &anchor());
            assert!(
                matches!(parsed, Err(ConfigError::Syntax(_))),
                "{text:?}: {parsed:?}"
            );
        }
    }

    // -- the codec is a bijection -----------------------------------------------------------

    fn segment() -> impl Strategy<Value = String> {
        "[a-z0-9._ \"\\\\é-]{1,8}".prop_filter("dot segment", |s| s != "." && s != "..")
    }

    fn abs_dirs() -> impl Strategy<Value = AbsDir> {
        vec(segment(), 1..4).prop_map(|parts| abs(&format!("/{}", parts.join("/"))))
    }

    fn home_rels() -> impl Strategy<Value = HomeRel> {
        vec(segment(), 1..4).prop_map(|parts| rel(&parts.join("/")))
    }

    fn bases() -> impl Strategy<Value = BaseSpec> {
        prop_oneof![
            "[A-Za-z0-9][A-Za-z0-9._-]{0,15}".prop_map(|name| preset(&name)),
            abs_dirs().prop_map(BaseSpec::Dir),
        ]
    }

    fn requests() -> impl Strategy<Value = MountRequest> {
        (abs_dirs(), prop_oneof![Just(Mode::Rw), Just(Mode::Ro)])
            .prop_map(|(path, mode)| MountRequest { path, mode })
    }

    fn ports() -> impl Strategy<Value = SshPort> {
        prop_oneof![
            Just(SshPort::Auto),
            (1024_u16..=65_535).prop_map(|p| SshPort::parse(p).unwrap()),
        ]
    }

    fn identities() -> impl Strategy<Value = (UserName, Uid, Gid)> {
        (
            "[a-z_][a-z0-9_-]{0,31}".prop_filter("root", |name| name != "root"),
            1..=u32::MAX - 1,
            1..=u32::MAX - 1,
        )
            .prop_map(|(name, uid, gid)| {
                (
                    user(&name),
                    Uid::parse(uid).unwrap(),
                    Gid::parse(gid).unwrap(),
                )
            })
    }

    fn configs() -> impl Strategy<Value = RunConfig> {
        (
            identities(),
            bases(),
            vec(requests(), 0..4),
            vec(home_rels(), 0..4),
            (ports(), 1..=86_400_u32, 1..=86_400_u32),
            (
                prop_oneof![Just(LingerPolicy::Enable), Just(LingerPolicy::Warn)],
                prop_oneof![Just(HomeStorage::Share), Just(HomeStorage::Volume)],
            ),
        )
            .prop_map(
                |((user, uid, gid), base, mounts, files, (port, boot, stop), (linger, storage))| {
                    RunConfig {
                        user,
                        uid,
                        gid,
                        base_nix_config: base,
                        default_mounts: mounts,
                        home_files: files,
                        ssh: SshConfig { port },
                        timeouts: TimeoutsConfig {
                            boot: secs(boot),
                            stop: secs(stop),
                        },
                        linger,
                        home_storage: storage,
                    }
                },
            )
    }

    fn keys_of(value: &Value) -> Vec<String> {
        match value {
            Value::Dict(doc) => doc.iter().map(|(key, _)| key.to_owned()).collect(),
            Value::Null
            | Value::Bool(_)
            | Value::Int(_)
            | Value::Float(_)
            | Value::Str(_)
            | Value::List(_) => Vec::new(),
        }
    }

    fn sorted(mut keys: Vec<String>) -> Vec<String> {
        keys.sort_unstable();
        keys
    }

    fn as_strings(keys: &[&str]) -> Vec<String> {
        keys.iter().map(|key| (*key).to_owned()).collect()
    }

    proptest! {
        #[test]
        fn decode_of_encode_is_the_identity(config in configs()) {
            let text = render(&config);
            prop_assert_eq!(parse(&text, &anchor()), Ok(config));
        }

        #[test]
        fn encode_of_decode_is_the_identity_on_canonical_text(config in configs()) {
            let text = render(&config);
            let decoded = parse(&text, &anchor()).unwrap();
            prop_assert_eq!(render(&decoded), text);
        }

        #[test]
        fn every_generated_file_has_every_key_of_every_block(config in configs()) {
            let doc = encode(&config);
            let top: Vec<String> = doc.iter().map(|(key, _)| key.to_owned()).collect();
            prop_assert_eq!(sorted(top), as_strings(Block::Root.known()));
            let get = |key: Key| doc.get(key.as_str()).unwrap();
            prop_assert_eq!(sorted(keys_of(get(Key::Ssh))), as_strings(Block::Ssh.known()));
            prop_assert_eq!(
                sorted(keys_of(get(Key::Timeouts))),
                as_strings(Block::Timeouts.known())
            );
            if let Value::List(items) = get(Key::DefaultMounts) {
                for item in items {
                    prop_assert_eq!(sorted(keys_of(item)), as_strings(Block::Mount.known()));
                }
            }
        }
    }

    #[test]
    fn the_generator_reaches_every_variant_of_the_enumerated_settings() {
        use proptest::strategy::ValueTree as _;
        use proptest::test_runner::TestRunner;
        let mut runner = TestRunner::deterministic();
        let strategy = configs();
        let seen: Vec<RunConfig> = (0..200)
            .map(|_| strategy.new_tree(&mut runner).unwrap().current())
            .collect();
        let any = |wanted: fn(&RunConfig) -> bool| seen.iter().any(wanted);
        assert!(any(|c| c.linger == LingerPolicy::Warn));
        assert!(any(|c| c.linger == LingerPolicy::Enable));
        assert!(any(|c| c.home_storage == HomeStorage::Volume));
        assert!(any(|c| c.home_storage == HomeStorage::Share));
        assert!(any(|c| c.ssh.port == SshPort::Auto));
        assert!(any(|c| c.ssh.port != SshPort::Auto));
        assert!(any(|c| matches!(c.base_nix_config, BaseSpec::Dir(_))));
        assert!(any(|c| matches!(c.base_nix_config, BaseSpec::Preset(_))));
        assert!(any(|c| c.default_mounts.iter().any(|m| m.mode == Mode::Ro)));
        assert!(any(|c| c.default_mounts.iter().any(|m| m.mode == Mode::Rw)));
        assert!(any(|c| c.default_mounts.is_empty()));
        assert!(any(|c| c.home_files.is_empty()));
        assert!(any(|c| !c.home_files.is_empty()));
    }
}
