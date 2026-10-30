//! `Shares`: the directories shared into one VM, checked as a set. Constructible only through
//! [`Shares::validate`]: observed directories, no duplicates, none covering the guest home, paths
//! the guest's mounts table can hold, at most [`MAX_SHARES`] of them (one tag each), parents
//! before the paths nested in them.

use std::collections::HashSet;

use crate::diagnostic::Diagnostic;
use crate::domain::abs_path::{AbsDir, HomeDir};
use crate::domain::mount::{HostDir, HostDirError, Mount, MountRequest};
use crate::domain::observed::Observation;
use crate::rails::NonEmpty;

/// The most directories one VM shares: tags are `d1..=d65535`.
pub const MAX_SHARES: usize = 65_535;

/// Validated mounts, parents first.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Shares(Vec<Mount>);

/// Why a requested mount, or the request list as a whole, is refused. Paths are the resolved ones
/// where the problem is about the directory itself and the requested one where nothing could be
/// resolved.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum MountProblem {
    /// About the whole list: `count` is the number of requests.
    #[error("can't share {count} directories: tags run out at {MAX_SHARES}")]
    TooMany { count: usize },
    #[error("{path}: given more than once")]
    Duplicate { path: AbsDir },
    #[error("{path}: not a directory")]
    NotADirectory { path: AbsDir },
    #[error("{path}: would hide the VM's home directory {home}")]
    HidesHome { path: AbsDir, home: HomeDir },
    /// The guest's `mounts` table is one tab-separated line per share.
    #[error("can't share {path}: its path contains a tab or a newline")]
    Unlistable { path: AbsDir },
}

impl Diagnostic for MountProblem {
    fn code(&self) -> &'static str {
        match self {
            Self::TooMany { .. } => "mounts.too-many",
            Self::Unlistable { .. } => "mounts.unlistable",
            Self::Duplicate { .. } => "mounts.duplicate",
            Self::NotADirectory { .. } => "mounts.not-a-directory",
            Self::HidesHome { .. } => "mounts.hides-home",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::TooMany { .. } => Some(format!("share at most {MAX_SHARES} directories")),
            Self::Unlistable { .. } => Some(
                "share a directory whose path has no tab or newline (rename it or share a parent)"
                    .to_owned(),
            ),
            Self::Duplicate { .. } => Some("list each directory once".to_owned()),
            Self::NotADirectory { .. } => {
                Some("create the directory, or give the path of an existing one".to_owned())
            }
            Self::HidesHome { home, .. } => Some(format!(
                "share a directory inside {home}, or one that doesn't contain it"
            )),
        }
    }
}

/// The non-empty components of an absolute normalised path; `/` has none.
fn components(path: &str) -> impl Iterator<Item = &str> {
    path.split('/').filter(|part| !part.is_empty())
}

/// `parent` is `child` or a directory above it.
fn covers(parent: &str, child: &str) -> bool {
    let above: Vec<&str> = components(parent).collect();
    let below: Vec<&str> = components(child).collect();
    below.starts_with(&above)
}

/// Remembers which paths were seen, as requested and as resolved.
#[derive(Default)]
struct Seen {
    asked: HashSet<AbsDir>,
    mounted: HashSet<AbsDir>,
}

fn check(
    request: &MountRequest,
    protected: &[HomeDir],
    probe: &impl Fn(&AbsDir) -> Observation,
    seen: &mut Seen,
) -> Result<Mount, MountProblem> {
    // Asked-for twice is a duplicate even when the directory is missing.
    if !seen.asked.insert(request.path.clone()) {
        return Err(MountProblem::Duplicate {
            path: request.path.clone(),
        });
    }
    let dir = HostDir::observe(&request.path, probe).map_err(|error| match error {
        HostDirError::NotADirectory { path } => MountProblem::NotADirectory { path },
    })?;
    // Two spellings (symlinks) of one directory.
    if !seen.mounted.insert(dir.path().clone()) {
        return Err(MountProblem::Duplicate {
            path: dir.path().clone(),
        });
    }
    if dir.path().get().contains(['\t', '\n']) {
        return Err(MountProblem::Unlistable {
            path: dir.path().clone(),
        });
    }
    if let Some(home) = protected
        .iter()
        .find(|home| covers(dir.path().get(), home.get()))
    {
        return Err(MountProblem::HidesHome {
            path: dir.path().clone(),
            home: home.clone(),
        });
    }
    Ok(Mount {
        dir,
        mode: request.mode,
    })
}

impl Shares {
    /// Observe every request through `probe` and check the set, reporting all problems at once
    /// (`TooMany` first, then one per refused request in request order); on success order parents
    /// first. At most [`MAX_SHARES`] requests, so every share gets a tag.
    ///
    /// `protected` are directories that must stay visible in the guest (its home): sharing one of
    /// them, or anything above one, would cover it up. Nested shares mount on top of their parent,
    /// so parents go first; paths compare component by component (`/a` < `/a/b` < `/a-b`).
    ///
    /// # Errors
    ///
    /// `TooMany` for more than [`MAX_SHARES`] requests, and one [`MountProblem`] per refused
    /// request.
    pub fn validate(
        requests: &[MountRequest],
        protected: &[HomeDir],
        probe: impl Fn(&AbsDir) -> Observation,
    ) -> Result<Self, NonEmpty<MountProblem>> {
        let mut seen = Seen::default();
        let mut valid = Vec::new();
        let mut problems = Vec::new();
        if requests.len() > MAX_SHARES {
            problems.push(MountProblem::TooMany {
                count: requests.len(),
            });
        }
        for request in requests {
            match check(request, protected, &probe, &mut seen) {
                Ok(mount) => valid.push(mount),
                Err(problem) => problems.push(problem),
            }
        }
        if let Some(problems) = NonEmpty::from_vec(problems) {
            return Err(problems);
        }
        valid.sort_by(|a, b| components(a.dir.path().get()).cmp(components(b.dir.path().get())));
        Ok(Self(valid))
    }

    #[must_use]
    pub fn as_slice(&self) -> &[Mount] {
        &self.0
    }

    pub fn iter(&self) -> impl Iterator<Item = &Mount> {
        self.0.iter()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl IntoIterator for Shares {
    type Item = Mount;
    type IntoIter = std::vec::IntoIter<Mount>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::domain::mode::Mode;
    use crate::domain::mount::tests::abs;
    use crate::testing::{self, Entry};

    fn home(text: &str) -> HomeDir {
        HomeDir::parse(text.to_owned()).unwrap()
    }

    fn witness(e: &MountProblem) -> &'static str {
        match e {
            MountProblem::TooMany { .. } => "TooMany",
            MountProblem::Unlistable { .. } => "Unlistable",
            MountProblem::Duplicate { .. } => "Duplicate",
            MountProblem::NotADirectory { .. } => "NotADirectory",
            MountProblem::HidesHome { .. } => "HidesHome",
        }
    }

    fn errors() -> Vec<MountProblem> {
        vec![
            MountProblem::TooMany { count: 65_536 },
            MountProblem::Unlistable {
                path: abs("/srv/a\tb"),
            },
            MountProblem::Duplicate {
                path: abs("/srv/a"),
            },
            MountProblem::NotADirectory {
                path: abs("/srv/x"),
            },
            MountProblem::HidesHome {
                path: abs("/home"),
                home: home("/home/u"),
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
            &[
                "TooMany",
                "Unlistable",
                "Duplicate",
                "NotADirectory",
                "HidesHome",
            ],
        );
    }

    #[test]
    fn the_tag_limit_is_the_largest_u16() {
        assert_eq!(MAX_SHARES, usize::from(u16::MAX));
    }

    #[test]
    fn problems_say_which_path_and_what_to_do() {
        testing::table(
            &[
                (
                    MountProblem::TooMany { count: 65_536 },
                    (
                        "can't share 65536 directories: tags run out at 65535".to_owned(),
                        "share at most 65535 directories".to_owned(),
                    ),
                ),
                (
                    MountProblem::Unlistable {
                        path: abs("/srv/a\tb"),
                    },
                    (
                        "can't share /srv/a\tb: its path contains a tab or a newline".to_owned(),
                        "share a directory whose path has no tab or newline (rename it or share \
                         a parent)"
                            .to_owned(),
                    ),
                ),
                (
                    MountProblem::Duplicate {
                        path: abs("/srv/a"),
                    },
                    (
                        "/srv/a: given more than once".to_owned(),
                        "list each directory once".to_owned(),
                    ),
                ),
                (
                    MountProblem::NotADirectory {
                        path: abs("/srv/x"),
                    },
                    (
                        "/srv/x: not a directory".to_owned(),
                        "create the directory, or give the path of an existing one".to_owned(),
                    ),
                ),
                (
                    MountProblem::HidesHome {
                        path: abs("/home"),
                        home: home("/home/u"),
                    },
                    (
                        "/home: would hide the VM's home directory /home/u".to_owned(),
                        "share a directory inside /home/u, or one that doesn't contain it"
                            .to_owned(),
                    ),
                ),
            ],
            |problem| (problem.message(), problem.hint().unwrap_or_default()),
        );
    }

    fn request(path: &str, mode: Mode) -> MountRequest {
        MountRequest {
            path: abs(path),
            mode,
        }
    }

    fn rw(path: &str) -> MountRequest {
        request(path, Mode::Rw)
    }

    /// Everything is a directory except `missing`; `links` resolve to their target.
    fn probe<'a>(
        missing: &'a [&'a str],
        links: &'a [(&'a str, &'a str)],
    ) -> impl Fn(&AbsDir) -> Observation + 'a {
        move |path| {
            let text = path.get().as_str();
            if missing.contains(&text) {
                return Observation::Missing;
            }
            let target = links
                .iter()
                .find(|(link, _)| *link == text)
                .map_or(text, |(_, target)| *target);
            Observation::Directory {
                resolved: abs(target),
            }
        }
    }

    type Outcome = Result<Vec<(String, Mode)>, Vec<MountProblem>>;

    fn outcome(shares: Result<Shares, NonEmpty<MountProblem>>) -> Outcome {
        shares
            .map(|s| {
                s.iter()
                    .map(|m| (m.dir.path().get().clone(), m.mode))
                    .collect()
            })
            .map_err(NonEmpty::into_vec)
    }

    #[allow(clippy::unnecessary_wraps, reason = "a table cell is a Result")]
    fn ok(list: &[(&str, Mode)]) -> Outcome {
        Ok(list.iter().map(|(p, m)| ((*p).to_owned(), *m)).collect())
    }

    fn dup(path: &str) -> MountProblem {
        MountProblem::Duplicate { path: abs(path) }
    }

    fn not_dir(path: &str) -> MountProblem {
        MountProblem::NotADirectory { path: abs(path) }
    }

    fn hides(path: &str, home_dir: &str) -> MountProblem {
        MountProblem::HidesHome {
            path: abs(path),
            home: home(home_dir),
        }
    }

    #[derive(Debug)]
    struct Case {
        requests: Vec<MountRequest>,
        protected: Vec<HomeDir>,
        missing: Vec<&'static str>,
        links: Vec<(&'static str, &'static str)>,
    }

    fn case(requests: Vec<MountRequest>, protected: &[&str]) -> Case {
        Case {
            requests,
            protected: protected.iter().map(|p| home(p)).collect(),
            missing: Vec::new(),
            links: Vec::new(),
        }
    }

    fn run(case: &Case) -> Outcome {
        outcome(Shares::validate(
            &case.requests,
            &case.protected,
            probe(&case.missing, &case.links),
        ))
    }

    #[test]
    fn valid_sets_are_accepted_and_ordered_parents_first() {
        testing::table(
            &[
                (case(vec![], &["/home/u"]), ok(&[])),
                (
                    case(vec![rw("/srv/a")], &["/home/u"]),
                    ok(&[("/srv/a", Mode::Rw)]),
                ),
                (
                    case(vec![rw("/srv/a"), rw("/srv/a/b")], &["/home/u"]),
                    ok(&[("/srv/a", Mode::Rw), ("/srv/a/b", Mode::Rw)]),
                ),
                (
                    case(
                        vec![request("/srv/a/b", Mode::Ro), rw("/srv/a")],
                        &["/home/u"],
                    ),
                    ok(&[("/srv/a", Mode::Rw), ("/srv/a/b", Mode::Ro)]),
                ),
                (
                    case(vec![rw("/srv/z"), rw("/srv/b"), rw("/srv/m")], &["/home/u"]),
                    ok(&[
                        ("/srv/b", Mode::Rw),
                        ("/srv/m", Mode::Rw),
                        ("/srv/z", Mode::Rw),
                    ]),
                ),
                (
                    case(
                        vec![rw("/srv/a-b"), rw("/srv/a/b"), rw("/srv/a"), rw("/srv/a.c")],
                        &["/home/u"],
                    ),
                    ok(&[
                        ("/srv/a", Mode::Rw),
                        ("/srv/a/b", Mode::Rw),
                        ("/srv/a-b", Mode::Rw),
                        ("/srv/a.c", Mode::Rw),
                    ]),
                ),
                (
                    case(
                        vec![rw("/a/b/c/d"), rw("/a"), rw("/a/b/c"), rw("/a/b")],
                        &["/home/u"],
                    ),
                    ok(&[
                        ("/a", Mode::Rw),
                        ("/a/b", Mode::Rw),
                        ("/a/b/c", Mode::Rw),
                        ("/a/b/c/d", Mode::Rw),
                    ]),
                ),
                (
                    case(vec![rw("/srv/z"), rw("/srv/é"), rw("/srv/a")], &["/home/u"]),
                    ok(&[
                        ("/srv/a", Mode::Rw),
                        ("/srv/z", Mode::Rw),
                        ("/srv/é", Mode::Rw),
                    ]),
                ),
                (
                    case(vec![rw("/home/u/dev"), rw("/home/u2")], &["/home/u"]),
                    ok(&[("/home/u/dev", Mode::Rw), ("/home/u2", Mode::Rw)]),
                ),
                (
                    case(vec![rw("/srv"), rw("/")], &[]),
                    ok(&[("/", Mode::Rw), ("/srv", Mode::Rw)]),
                ),
                (
                    case(
                        vec![request("/srv/b", Mode::Ro), rw("/srv/a")],
                        &["/home/u"],
                    ),
                    ok(&[("/srv/a", Mode::Rw), ("/srv/b", Mode::Ro)]),
                ),
            ],
            run,
        );
    }

    #[test]
    fn duplicates_missing_directories_and_hidden_homes_are_each_reported() {
        let mut missing_file = case(vec![rw("/srv/a"), rw("/srv/file")], &["/home/u"]);
        missing_file.missing = vec!["/srv/file"];
        let mut twice_missing = case(vec![rw("/srv/x"), rw("/srv/x")], &["/home/u"]);
        twice_missing.missing = vec!["/srv/x"];
        let mut everything = case(
            vec![
                rw("/srv/a"),
                rw("/srv/a"),
                rw("/srv/file"),
                rw("/home"),
                rw("/ok"),
            ],
            &["/home/u"],
        );
        everything.missing = vec!["/srv/file"];
        testing::table(
            &[
                (
                    case(vec![rw("/srv/a"), rw("/srv/a")], &["/home/u"]),
                    Err(vec![dup("/srv/a")]),
                ),
                (
                    case(
                        vec![rw("/srv/a"), request("/srv/a", Mode::Ro)],
                        &["/home/u"],
                    ),
                    Err(vec![dup("/srv/a")]),
                ),
                (
                    case(vec![rw("/srv/a"), rw("/srv/a"), rw("/srv/a")], &["/home/u"]),
                    Err(vec![dup("/srv/a"), dup("/srv/a")]),
                ),
                (missing_file, Err(vec![not_dir("/srv/file")])),
                (twice_missing, Err(vec![not_dir("/srv/x"), dup("/srv/x")])),
                (
                    case(vec![rw("/home/u")], &["/home/u"]),
                    Err(vec![hides("/home/u", "/home/u")]),
                ),
                (
                    case(vec![rw("/home")], &["/home/u"]),
                    Err(vec![hides("/home", "/home/u")]),
                ),
                (
                    case(vec![rw("/")], &["/home/u"]),
                    Err(vec![hides("/", "/home/u")]),
                ),
                (
                    case(vec![rw("/home")], &["/home/u", "/home/v"]),
                    Err(vec![hides("/home", "/home/u")]),
                ),
                (
                    case(vec![rw("/srv")], &["/home/u", "/srv/v"]),
                    Err(vec![hides("/srv", "/srv/v")]),
                ),
                (
                    everything,
                    Err(vec![
                        dup("/srv/a"),
                        not_dir("/srv/file"),
                        hides("/home", "/home/u"),
                    ]),
                ),
            ],
            run,
        );
    }

    fn unlistable(path: &str) -> MountProblem {
        MountProblem::Unlistable { path: abs(path) }
    }

    #[test]
    fn paths_the_mounts_table_cannot_hold_are_refused_with_the_other_problems() {
        let mut linked = case(vec![rw("/srv/link"), rw("/srv/a")], &["/home/u"]);
        linked.links = vec![("/srv/link", "/srv/t\tab")];
        testing::table(
            &[
                (
                    case(vec![rw("/srv/a\tb")], &[]),
                    Err(vec![unlistable("/srv/a\tb")]),
                ),
                (
                    case(vec![rw("/srv/a\nb")], &[]),
                    Err(vec![unlistable("/srv/a\nb")]),
                ),
                (
                    case(vec![rw("/srv/a\rb"), rw("/srv/a b")], &[]),
                    ok(&[("/srv/a\rb", Mode::Rw), ("/srv/a b", Mode::Rw)]),
                ),
                // Judged on the resolved path, which is what the table lists.
                (linked, Err(vec![unlistable("/srv/t\tab")])),
                (
                    case(
                        vec![rw("/srv/a\tb"), rw("/srv/a"), rw("/srv/a"), rw("/home")],
                        &["/home/u"],
                    ),
                    Err(vec![
                        unlistable("/srv/a\tb"),
                        dup("/srv/a"),
                        hides("/home", "/home/u"),
                    ]),
                ),
            ],
            run,
        );
    }

    /// `count` distinct directories `/m/dN`.
    fn many(count: usize) -> Vec<MountRequest> {
        (0..count).map(|n| rw(&format!("/m/d{n}"))).collect()
    }

    #[test]
    fn the_most_that_has_tags_is_accepted_and_one_more_is_reported_before_the_rest() {
        let all = Shares::validate(&many(MAX_SHARES), &[], probe(&[], &[])).unwrap();
        assert_eq!(all.len(), MAX_SHARES);
        let mut requests = many(MAX_SHARES + 1);
        requests.push(rw("/m/d0"));
        let problems = Shares::validate(&requests, &[], probe(&["/m/d7"], &[]))
            .unwrap_err()
            .into_vec();
        assert_eq!(
            problems,
            [
                MountProblem::TooMany {
                    count: MAX_SHARES + 2
                },
                not_dir("/m/d7"),
                dup("/m/d0"),
            ]
        );
        assert_eq!(
            Shares::validate(&many(MAX_SHARES + 1), &[], probe(&[], &[]))
                .unwrap_err()
                .into_vec(),
            [MountProblem::TooMany {
                count: MAX_SHARES + 1
            }]
        );
    }

    #[test]
    fn symlinks_are_resolved_before_duplicates_and_hidden_homes_are_judged() {
        let mut same_target = case(vec![rw("/srv/a"), rw("/srv/link")], &["/home/u"]);
        same_target.links = vec![("/srv/link", "/srv/a")];
        let mut into_home = case(vec![rw("/srv/link")], &["/home/u"]);
        into_home.links = vec![("/srv/link", "/home/u")];
        let mut reordered = case(vec![rw("/srv/b-link"), rw("/srv/a")], &["/home/u"]);
        reordered.links = vec![("/srv/b-link", "/srv/a/b")];
        testing::table(
            &[
                (same_target, Err(vec![dup("/srv/a")])),
                (into_home, Err(vec![hides("/home/u", "/home/u")])),
                (
                    reordered,
                    ok(&[("/srv/a", Mode::Rw), ("/srv/a/b", Mode::Rw)]),
                ),
            ],
            run,
        );
    }

    #[test]
    fn the_validated_set_exposes_its_mounts_in_order() {
        let shares = Shares::validate(
            &[rw("/srv/b"), request("/srv/a", Mode::Ro)],
            &[],
            probe(&[], &[]),
        )
        .unwrap();
        let specs: Vec<String> = shares.iter().map(Mount::spec).collect();
        assert_eq!(specs, ["/srv/a:ro", "/srv/b:rw"]);
        assert_eq!(shares.len(), 2);
        assert!(!shares.is_empty());
        assert_eq!(shares.as_slice().len(), 2);
        let owned: Vec<String> = shares.into_iter().map(|m| m.spec()).collect();
        assert_eq!(owned, ["/srv/a:ro", "/srv/b:rw"]);
        let none = Shares::validate(&[], &[], probe(&[], &[])).unwrap();
        assert!(none.is_empty());
        assert_eq!(none.len(), 0);
    }

    fn field<'a>(value: &'a serde_json::Value, key: &str) -> &'a serde_json::Value {
        value.get(key).expect("corpus field")
    }

    fn text<'a>(value: &'a serde_json::Value, key: &str) -> &'a str {
        field(value, key).as_str().expect("corpus string")
    }

    fn list<'a>(value: &'a serde_json::Value, key: &str) -> &'a [serde_json::Value] {
        field(value, key).as_array().expect("corpus array")
    }

    fn corpus_mount(value: &serde_json::Value) -> (String, Mode) {
        let mode = Mode::from_suffix(text(value, "mode")).expect("corpus mode");
        (text(value, "path").to_owned(), mode)
    }

    fn corpus_problem(kind: &str, path: &str, home_dir: Option<&str>) -> Option<MountProblem> {
        match (kind, home_dir) {
            ("duplicate", _) => Some(dup(path)),
            ("not_a_directory", _) => Some(not_dir(path)),
            ("hides_home", Some(h)) => Some(hides(path, h)),
            _ => None,
        }
    }

    /// The Python implementation's behaviour (`tests/golden/mount_order.json`).
    #[test]
    fn golden_mount_order_matches_the_python_implementation() {
        let corpus = include_str!("../../tests/golden/mount_order.json");
        let cases: Vec<serde_json::Value> = serde_json::from_str(corpus).unwrap();
        assert!(cases.len() > 20, "the corpus is loaded");
        for case in &cases {
            let name = text(case, "name");
            let input = field(case, "input");
            let strings = |key: &str| -> Vec<String> {
                list(input, key)
                    .iter()
                    .map(|v| v.as_str().expect("corpus string").to_owned())
                    .collect()
            };
            let protected: Vec<HomeDir> = strings("protected").iter().map(|p| home(p)).collect();
            let not_dirs = strings("not_dirs");
            let requests: Vec<MountRequest> = list(input, "mounts")
                .iter()
                .map(|m| {
                    let (path, mode) = corpus_mount(m);
                    request(&path, mode)
                })
                .collect();
            let got = Shares::validate(&requests, &protected, |path| {
                if not_dirs.contains(path.get()) {
                    Observation::Missing
                } else {
                    Observation::Directory {
                        resolved: path.clone(),
                    }
                }
            });
            let output = field(case, "output");
            let expected: Outcome = match output.get("ok") {
                Some(_) => Ok(list(output, "ok").iter().map(corpus_mount).collect()),
                None => Err(list(output, "errors")
                    .iter()
                    .map(|e| {
                        let home_dir = e.get("home").and_then(serde_json::Value::as_str);
                        corpus_problem(text(e, "kind"), text(e, "path"), home_dir)
                            .expect("known corpus problem")
                    })
                    .collect()),
            };
            assert_eq!(outcome(got), expected, "corpus case {name}");
        }
    }

    const NAMES: [&str; 5] = ["a", "b", "a-b", "a.c", "é"];

    fn path_text(parts: &[usize]) -> String {
        let names: Vec<&str> = parts
            .iter()
            .filter_map(|&i| NAMES.get(i).copied())
            .collect();
        format!("/{}", names.join("/"))
    }

    fn paths() -> impl Strategy<Value = String> {
        prop::collection::vec(0..NAMES.len(), 0..4).prop_map(|parts| path_text(&parts))
    }

    /// `ancestor` is a strict ancestor of `path`, by text.
    fn strictly_above(ancestor: &str, path: &str) -> bool {
        ancestor != path
            && (ancestor == "/"
                || path
                    .strip_prefix(ancestor)
                    .is_some_and(|r| r.starts_with('/')))
    }

    /// What `validate` must report for request `i`, judged from the list alone.
    fn expected_problem(
        i: usize,
        paths: &[String],
        missing: &[String],
        protected: &[String],
    ) -> Option<MountProblem> {
        let path = paths.get(i)?;
        if paths.iter().take(i).any(|earlier| earlier == path) {
            Some(dup(path))
        } else if missing.contains(path) {
            Some(not_dir(path))
        } else {
            protected
                .iter()
                .find(|h| *h == path || strictly_above(path, h))
                .map(|h| hides(path, h))
        }
    }

    proptest! {
        #[test]
        fn output_is_ordered_parents_first_and_every_problem_is_reported(
            paths in prop::collection::vec(paths(), 0..8),
            missing in prop::collection::vec(paths(), 0..3),
            protected in prop::collection::vec(paths(), 0..3),
            ro in prop::collection::vec(any::<bool>(), 8),
        ) {
            let requests: Vec<MountRequest> = paths
                .iter()
                .zip(&ro)
                .map(|(p, &r)| request(p, if r { Mode::Ro } else { Mode::Rw }))
                .collect();
            let homes: Vec<HomeDir> = protected.iter().map(|p| home(p)).collect();
            let result = Shares::validate(&requests, &homes, |path| {
                if missing.contains(path.get()) {
                    Observation::Missing
                } else {
                    Observation::Directory { resolved: path.clone() }
                }
            });
            let expected: Vec<MountProblem> = (0..paths.len())
                .filter_map(|i| expected_problem(i, &paths, &missing, &protected))
                .collect();
            match result {
                Err(problems) => prop_assert_eq!(problems.into_vec(), expected),
                Ok(shares) => {
                    prop_assert_eq!(expected, Vec::<MountProblem>::new());
                    let out: Vec<&str> =
                        shares.iter().map(|m| m.dir.path().get().as_str()).collect();
                    for (i, later) in out.iter().enumerate() {
                        for earlier in out.iter().take(i) {
                            let message = format!("{later} listed after {earlier}");
                            prop_assert!(!strictly_above(later, earlier), "{}", message);
                        }
                    }
                    let mut sorted_in: Vec<&str> = paths.iter().map(String::as_str).collect();
                    let mut sorted_out = out.clone();
                    sorted_in.sort_unstable();
                    sorted_out.sort_unstable();
                    prop_assert_eq!(sorted_in, sorted_out);
                }
            }
        }
    }
}
