//! Test support (design §Testing): table runner, temp dir guard, witness and catalogue helpers.

#![allow(
    clippy::disallowed_types,
    reason = "boundary: test fixtures create raw temp paths"
)]

use std::collections::BTreeSet;
use std::fmt::{Debug, Write as _};
use std::fs;
use std::io;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::diagnostic::Diagnostic;

pub mod fakes;
pub mod sweep;

/// Held while a test copies or starts executables, or keeps a lock descriptor inheritable: a fork
/// elsewhere would otherwise carry the copy's write descriptor (`ETXTBSY` on exec) or the lock
/// into its child.
pub fn one_spawn_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    static SPAWNING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    SPAWNING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Asserts `run(input) == expected` for every case, naming the failing case's index.
#[track_caller]
pub fn table<I: Debug, O: PartialEq + Debug>(cases: &[(I, O)], run: impl Fn(&I) -> O) {
    for (index, (input, expected)) in cases.iter().enumerate() {
        assert_eq!(&run(input), expected, "case {index}: input {input:?}");
    }
}

/// A fresh directory under `env::temp_dir()`, removed with its contents on drop.
#[derive(Debug)]
pub struct TempDir(PathBuf);

impl TempDir {
    /// Creates `amoeboid-test-PID-N-LABEL` with mode 0700; never reuses an existing directory.
    pub fn new(label: &str) -> io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("amoeboid-test-{}-{n}-{label}", process::id()));
        fs::DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Self(path))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // Best effort: a test that fails to clean up must not hide its own failure.
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Asserts that `cases` hold at least one value of every variant in `variants`.
///
/// `witness` matches every variant without `_` and returns its name, so a new variant does not
/// compile until it has an arm; a name missing from `variants` or without a case fails here.
#[track_caller]
pub fn every_variant<E: Debug>(cases: &[E], witness: fn(&E) -> &'static str, variants: &[&str]) {
    let known: BTreeSet<&str> = variants.iter().copied().collect();
    assert_eq!(
        known.len(),
        variants.len(),
        "variant names listed twice: {variants:?}"
    );
    let mut seen = BTreeSet::new();
    for (index, case) in cases.iter().enumerate() {
        let name = witness(case);
        assert!(
            known.contains(name),
            "case {index}: variant {name} not listed in {variants:?}"
        );
        seen.insert(name);
    }
    let missing: Vec<&str> = known.difference(&seen).copied().collect();
    assert!(missing.is_empty(), "variants without a case: {missing:?}");
}

/// One rendered diagnostic in the catalogue (design §Testing: diagnostic catalogue).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Entry {
    pub code: &'static str,
    pub message: String,
    pub hint: Option<String>,
}

impl Entry {
    /// The catalogue line of one diagnostic value.
    pub fn of(diagnostic: &impl Diagnostic) -> Self {
        Self {
            code: diagnostic.code(),
            message: diagnostic.message(),
            hint: diagnostic.hint(),
        }
    }
}

/// Renders entries in the given order; `Err` lists every code that occurs more than once.
pub fn catalogue(entries: &[Entry]) -> Result<String, Vec<&'static str>> {
    let mut seen = BTreeSet::new();
    let duplicates: BTreeSet<&'static str> = entries
        .iter()
        .filter(|e| !seen.insert(e.code))
        .map(|e| e.code)
        .collect();
    if !duplicates.is_empty() {
        return Err(duplicates.into_iter().collect());
    }
    let mut text = String::new();
    for entry in entries {
        let _ = writeln!(text, "{}\n  message: {}", entry.code, entry.message);
        if let Some(hint) = &entry.hint {
            let _ = writeln!(text, "  hint: {hint}");
        }
    }
    Ok(text)
}

/// Writes `text` to `tests/golden/REL` of this crate; changes show up as a reviewed git diff.
pub fn write_golden(rel: &str, text: &str) -> io::Result<()> {
    let path = golden_path(rel);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(&path, text)
}

/// `tests/golden/REL` of this crate.
pub fn golden_path(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(rel)
}

/// One golden pair: the texts of `DIR/NAME.<input ext>` and `DIR/NAME.<output ext>`.
#[derive(Debug)]
pub struct GoldenCase {
    pub name: String,
    pub input: String,
    pub output: String,
}

/// Every pair in `tests/golden/DIR` that has an input file, sorted by name; an input without
/// its output file is an error, so a half-written corpus can't pass.
pub fn golden_cases(dir: &str, input_ext: &str, output_ext: &str) -> io::Result<Vec<GoldenCase>> {
    golden_names(dir, input_ext)?
        .into_iter()
        .map(|name| {
            let read = |ext: &str| fs::read_to_string(golden_path(&format!("{dir}/{name}.{ext}")));
            Ok(GoldenCase {
                input: read(input_ext)?,
                output: read(output_ext)?,
                name,
            })
        })
        .collect()
}

/// One golden pair of raw bytes (inputs that need not be UTF-8).
#[derive(Debug)]
pub struct GoldenBytes {
    pub name: String,
    pub input: Vec<u8>,
    pub output: Vec<u8>,
}

/// [`golden_cases`] for raw bytes.
pub fn golden_byte_cases(
    dir: &str,
    input_ext: &str,
    output_ext: &str,
) -> io::Result<Vec<GoldenBytes>> {
    golden_names(dir, input_ext)?
        .into_iter()
        .map(|name| {
            let read = |ext: &str| fs::read(golden_path(&format!("{dir}/{name}.{ext}")));
            Ok(GoldenBytes {
                input: read(input_ext)?,
                output: read(output_ext)?,
                name,
            })
        })
        .collect()
}

/// The stems of `tests/golden/DIR/*.<input ext>`, sorted.
fn golden_names(dir: &str, input_ext: &str) -> io::Result<BTreeSet<String>> {
    let suffix = format!(".{input_ext}");
    let mut names = BTreeSet::new();
    for entry in fs::read_dir(golden_path(dir))? {
        let file = entry?.file_name();
        if let Some(stem) = file.to_string_lossy().strip_suffix(&suffix) {
            names.insert(stem.to_owned());
        }
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic;

    fn panic_message(run: impl FnOnce() + panic::UnwindSafe) -> String {
        let payload = panic::catch_unwind(run).expect_err("expected a panic");
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
            .unwrap_or_default()
    }

    #[test]
    fn table_passes_when_every_case_matches() {
        table(&[(1, 2), (2, 4)], |x| x * 2);
    }

    #[test]
    fn table_failure_names_the_case_index_and_input() {
        let message = panic_message(|| table(&[(1, 2), (3, 7)], |x| x * 2));
        assert!(message.contains("case 1: input 3"), "{message}");
    }

    #[test]
    fn temp_dirs_are_distinct_private_and_removed_on_drop() {
        use std::os::unix::fs::PermissionsExt as _;
        let a = TempDir::new("a").unwrap();
        let b = TempDir::new("a").unwrap();
        assert_ne!(a.path(), b.path());
        let mode = fs::metadata(a.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        fs::write(a.path().join("f"), "x").unwrap();
        let path = a.path().to_owned();
        drop(a);
        assert!(!path.exists());
    }

    #[derive(Debug)]
    enum Sample {
        One,
        Two,
    }

    fn witness(e: &Sample) -> &'static str {
        match e {
            Sample::One => "One",
            Sample::Two => "Two",
        }
    }

    #[test]
    fn every_variant_accepts_a_table_covering_all_variants() {
        every_variant(&[Sample::Two, Sample::One], witness, &["One", "Two"]);
    }

    #[test]
    fn every_variant_reports_variants_without_a_case() {
        let message = panic_message(|| every_variant(&[Sample::One], witness, &["One", "Two"]));
        assert!(
            message.contains(r#"variants without a case: ["Two"]"#),
            "{message}"
        );
    }

    #[test]
    fn every_variant_reports_a_variant_missing_from_the_list() {
        let message = panic_message(|| every_variant(&[Sample::Two], witness, &["One"]));
        assert!(
            message.contains("case 0: variant Two not listed"),
            "{message}"
        );
    }

    #[test]
    fn every_variant_reports_names_listed_twice() {
        let message = panic_message(|| every_variant(&[Sample::One], witness, &["One", "One"]));
        assert!(message.contains("variant names listed twice"), "{message}");
    }

    fn entry(code: &'static str, hint: Option<&str>) -> Entry {
        Entry {
            code,
            message: format!("can't {code}"),
            hint: hint.map(str::to_owned),
        }
    }

    #[test]
    fn catalogue_renders_code_message_and_hint_in_order() {
        let entries = [entry("b.x", Some("do y")), entry("a.x", None)];
        let expected = "b.x\n  message: can't b.x\n  hint: do y\na.x\n  message: can't a.x\n";
        assert_eq!(catalogue(&entries), Ok(expected.to_owned()));
    }

    #[test]
    fn catalogue_refuses_every_duplicate_code() {
        let entries = [
            entry("a", None),
            entry("b", None),
            entry("a", None),
            entry("b", None),
        ];
        assert_eq!(catalogue(&entries), Err(vec!["a", "b"]));
    }

    #[test]
    fn golden_cases_pairs_inputs_with_outputs_sorted_by_name() {
        let cases = golden_cases("kdl", "json", "kdl").unwrap();
        let names: Vec<&str> = cases.iter().map(|c| c.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted);
        assert!(names.contains(&"scalars"));
        let scalars = cases.iter().find(|c| c.name == "scalars").unwrap();
        assert!(scalars.input.contains("\"dict\""));
        assert!(scalars.output.starts_with("text \"hello\"\n"));
    }

    #[test]
    fn golden_cases_refuses_an_input_without_its_output() {
        assert!(golden_cases("kdl", "json", "no-such-ext").is_err());
    }

    #[test]
    fn write_golden_writes_under_tests_golden() {
        let rel = format!("testing-selftest-{}.txt", process::id());
        let path = golden_path(&rel);
        assert_eq!(
            path.parent(),
            Some(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/golden")
                    .as_path()
            )
        );
        write_golden(&rel, "x\n").unwrap();
        let text = fs::read_to_string(&path).unwrap();
        fs::remove_file(&path).unwrap();
        assert_eq!(text, "x\n");
    }
}
