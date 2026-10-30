//! The data model of a KDL document: `Value` trees under an ordered `Document` (design §Workspace
//! `kdl/`). Pure data; text conversion lives in `emit` and the parser adapter.

use std::fmt;

use crate::diagnostic::Diagnostic;

/// An integer proven to survive `emit` then `parse`: every `i128` but the smallest, which the
/// parser cannot read back (it reads the digits as a positive number before negating).
///
/// Not a `Refined`: besides [`Int::parse`] it has the total [`Int::from_i64`], since every `i64`
/// satisfies the predicate (design §Where values come from: derived values are never re-parsed).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Int(i128);

impl Int {
    /// # Errors
    /// `Smallest` for `i128::MIN`.
    pub fn parse(value: i128) -> Result<Self, IntError> {
        match value {
            i128::MIN => Err(IntError::Smallest),
            _ => Ok(Self(value)),
        }
    }

    /// Total: every `i64` is above `i128::MIN`.
    #[must_use]
    pub fn from_i64(value: i64) -> Self {
        Self(i128::from(value))
    }

    /// Total: every `u64` is above `i128::MIN`.
    #[must_use]
    pub fn from_u64(value: u64) -> Self {
        Self(i128::from(value))
    }

    #[must_use]
    pub const fn get(&self) -> &i128 {
        &self.0
    }

    #[must_use]
    pub const fn into_inner(self) -> i128 {
        self.0
    }
}

/// Shows the number only, like the refined types.
impl fmt::Debug for Int {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum IntError {
    #[error("integer {} can't be read back from KDL text", i128::MIN)]
    Smallest,
}

impl Diagnostic for IntError {
    fn code(&self) -> &'static str {
        match self {
            Self::Smallest => "kdl.int.smallest",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::Smallest => Some("use a larger integer".to_owned()),
        }
    }
}

/// A double compared by bit pattern, so `Value` can be `Eq`: `-0.0` differs from `0.0` and every
/// NaN is the one canonical NaN (KDL has a single `#nan`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Float(u64);

impl Float {
    #[must_use]
    pub fn new(value: f64) -> Self {
        Self(if value.is_nan() {
            f64::NAN.to_bits()
        } else {
            value.to_bits()
        })
    }

    #[must_use]
    pub fn get(self) -> f64 {
        f64::from_bits(self.0)
    }
}

/// One KDL value. Integers cover the whole range the parser reads (see [`Int`]).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Value {
    Null,
    Bool(bool),
    Int(Int),
    Float(Float),
    Str(String),
    List(Vec<Value>),
    Dict(Document),
}

/// Keys with their values in insertion order, each key at most once.
///
/// The top-level document and every `Value::Dict` are this type; the emitter and the parser keep
/// the order, so a text that parses emits back byte for byte.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Document(Vec<(String, Value)>);

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum DocumentError {
    #[error("duplicate key {key:?}")]
    DuplicateKey { key: String },
}

impl Diagnostic for DocumentError {
    fn code(&self) -> &'static str {
        match self {
            Self::DuplicateKey { .. } => "kdl.duplicate-key",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::DuplicateKey { .. } => Some("remove or rename one of the entries".to_owned()),
        }
    }
}

impl Document {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends an entry; a key already present is refused and the document is unchanged.
    ///
    /// # Errors
    /// `DuplicateKey` when `key` is already in the document.
    pub fn insert(&mut self, key: impl Into<String>, value: Value) -> Result<(), DocumentError> {
        let key = key.into();
        if self.get(&key).is_some() {
            return Err(DocumentError::DuplicateKey { key });
        }
        self.0.push((key, value));
        Ok(())
    }

    /// Sets `key` to `value`: replaces the value in place when `key` is present, else appends.
    pub fn set(&mut self, key: impl Into<String>, value: Value) {
        let key = key.into();
        match self.0.iter_mut().find(|(k, _)| *k == key) {
            Some((_, old)) => *old = value,
            None => self.0.push((key, value)),
        }
    }

    /// The document of `entries` in order.
    ///
    /// # Errors
    /// `DuplicateKey` for the first key that repeats.
    pub fn from_entries(
        entries: impl IntoIterator<Item = (String, Value)>,
    ) -> Result<Self, DocumentError> {
        let mut doc = Self::new();
        for (key, value) in entries {
            doc.insert(key, value)?;
        }
        Ok(doc)
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v))
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

impl IntoIterator for Document {
    type Item = (String, Value);
    type IntoIter = std::vec::IntoIter<(String, Value)>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use super::*;
    use crate::testing::{self, Entry};

    fn witness(e: &DocumentError) -> &'static str {
        match e {
            DocumentError::DuplicateKey { .. } => "DuplicateKey",
        }
    }

    fn errors() -> Vec<DocumentError> {
        vec![DocumentError::DuplicateKey {
            key: "a".to_owned(),
        }]
    }

    fn int_witness(e: &IntError) -> &'static str {
        match e {
            IntError::Smallest => "Smallest",
        }
    }

    fn int_errors() -> Vec<IntError> {
        vec![IntError::Smallest]
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        let mut all: Vec<Entry> = errors().iter().map(Entry::of).collect();
        all.extend(int_errors().iter().map(Entry::of));
        all
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(&errors(), witness, &["DuplicateKey"]);
        testing::every_variant(&int_errors(), int_witness, &["Smallest"]);
    }

    #[test]
    fn int_parse_accepts_every_integer_but_the_smallest() {
        testing::table(
            &[
                (i128::MIN, Err(IntError::Smallest)),
                (
                    -170_141_183_460_469_231_731_687_303_715_884_105_727,
                    Ok(-170_141_183_460_469_231_731_687_303_715_884_105_727),
                ),
                (-1, Ok(-1)),
                (0, Ok(0)),
                (1, Ok(1)),
                (i128::MAX, Ok(i128::MAX)),
            ],
            |value| Int::parse(*value).map(Int::into_inner),
        );
    }

    #[test]
    fn every_i64_is_an_int_with_the_same_value() {
        testing::table(
            &[
                (i64::MIN, i128::from(i64::MIN)),
                (-1, -1),
                (0, 0),
                (1, 1),
                (i64::MAX, i128::from(i64::MAX)),
            ],
            |value| Int::from_i64(*value).into_inner(),
        );
        assert_eq!(Int::parse(7), Ok(Int::from_i64(7)));
        testing::table(&[(0, 0), (u64::MAX, i128::from(u64::MAX))], |value| {
            Int::from_u64(*value).into_inner()
        });
    }

    #[test]
    fn ints_compare_and_format_like_their_number() {
        let (low, high) = (Int::from_i64(-2), Int::from_i64(3));
        assert!(low < high);
        assert_eq!(*low.get(), -2);
        assert_eq!(format!("{low:?} {high:?}"), "-2 3");
    }

    #[test]
    fn the_smallest_integer_is_refused_with_a_message_that_names_it() {
        assert_eq!(
            IntError::Smallest.message(),
            "integer -170141183460469231731687303715884105728 can't be read back from KDL text"
        );
    }

    fn int(n: i128) -> Value {
        Value::Int(Int::parse(n).unwrap())
    }

    fn doc(entries: &[(&str, Value)]) -> Document {
        Document::from_entries(entries.iter().map(|(k, v)| ((*k).to_owned(), v.clone()))).unwrap()
    }

    #[test]
    fn floats_compare_by_bits_with_one_canonical_nan() {
        let other_nan = f64::from_bits(f64::NAN.to_bits() | 1);
        assert!(other_nan.is_nan());
        testing::table(
            &[
                ((0.0, 0.0), true),
                ((0.0, -0.0), false),
                ((-0.0, -0.0), true),
                ((f64::NAN, other_nan), true),
                ((f64::NAN, f64::INFINITY), false),
                ((1.5, 1.5), true),
                ((1.5, -1.5), false),
            ],
            |(a, b)| Float::new(*a) == Float::new(*b),
        );
    }

    #[test]
    fn float_get_returns_the_value_it_was_made_from() {
        for value in [
            0.0,
            1.5,
            f64::MIN_POSITIVE,
            f64::MAX,
            f64::NEG_INFINITY,
            f64::INFINITY,
        ] {
            assert_eq!(Float::new(value).get().to_bits(), value.to_bits());
        }
        assert!(Float::new(-0.0).get().is_sign_negative());
        assert!(Float::new(f64::NAN).get().is_nan());
    }

    #[test]
    fn documents_keep_insertion_order() {
        let d = doc(&[("b", int(1)), ("a", int(2)), ("c", Value::Null)]);
        let keys: Vec<&str> = d.iter().map(|(k, _)| k).collect();
        assert_eq!(keys, ["b", "a", "c"]);
        assert_eq!(d.len(), 3);
        assert!(!d.is_empty());
        assert_eq!(
            d.into_iter().collect::<Vec<_>>(),
            [
                ("b".to_owned(), int(1)),
                ("a".to_owned(), int(2)),
                ("c".to_owned(), Value::Null),
            ]
        );
    }

    #[test]
    fn an_empty_document_has_no_entries() {
        let d = Document::new();
        assert!(d.is_empty());
        assert_eq!(d.len(), 0);
        assert_eq!(d.iter().count(), 0);
        assert_eq!(d, Document::default());
    }

    #[test]
    fn get_finds_a_key_exactly() {
        let d = doc(&[("a", int(1)), ("A", int(2)), ("", int(3))]);
        testing::table(
            &[
                ("a", Some(int(1))),
                ("A", Some(int(2))),
                ("", Some(int(3))),
                ("b", None),
                ("a ", None),
            ],
            |key| d.get(key).cloned(),
        );
    }

    #[test]
    fn a_repeated_key_is_refused_and_leaves_the_document_unchanged() {
        let mut d = doc(&[("a", int(1))]);
        assert_eq!(
            d.insert("a", int(2)),
            Err(DocumentError::DuplicateKey {
                key: "a".to_owned()
            })
        );
        assert_eq!(d, doc(&[("a", int(1))]));
        assert_eq!(d.insert("b", int(2)), Ok(()));
        assert_eq!(d, doc(&[("a", int(1)), ("b", int(2))]));
    }

    #[test]
    fn set_replaces_a_present_key_in_place_and_appends_a_new_one() {
        let mut d = doc(&[("a", int(1)), ("b", int(2))]);
        d.set("a", int(3));
        assert_eq!(d, doc(&[("a", int(3)), ("b", int(2))]));
        d.set("c", Value::Null);
        assert_eq!(d, doc(&[("a", int(3)), ("b", int(2)), ("c", Value::Null)]));
    }

    #[test]
    fn from_entries_reports_the_first_repeated_key() {
        let entry = |k: &str| (k.to_owned(), Value::Null);
        testing::table(
            &[
                (vec![], true),
                (vec!["a", "b"], true),
                (vec!["a", "a"], false),
                (vec!["a", "b", "b", "a"], false),
            ],
            |keys| Document::from_entries(keys.iter().map(|k| entry(k))).is_ok(),
        );
        assert_eq!(
            Document::from_entries(["a", "b", "b", "a"].map(entry)),
            Err(DocumentError::DuplicateKey {
                key: "b".to_owned()
            })
        );
    }

    #[test]
    fn nested_values_compare_structurally() {
        let a = Value::List(vec![Value::Dict(doc(&[("k", int(1))])), Value::Null]);
        let b = Value::List(vec![Value::Dict(doc(&[("k", int(1))])), Value::Null]);
        let c = Value::List(vec![Value::Dict(doc(&[("k", int(2))])), Value::Null]);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_ne!(Value::List(vec![]), Value::Dict(Document::new()));
    }
}
