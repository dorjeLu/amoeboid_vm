//! Refinement types (design §Refinement types): a base type plus a predicate, constructible only
//! by checking the predicate. Holding a [`Refined`] is the proof; nothing downstream re-checks.
//!
//! One marker type per domain concept, even when two predicates coincide, so the types stay
//! distinct:
//!
//! ```
//! use std::convert::Infallible;
//! use amoeboid::refined::{Refined, Refinement};
//!
//! struct Uid;
//! struct Gid;
//! impl Refinement<u32> for Uid {
//!     type Error = Infallible;
//!     fn check(_: &u32) -> Result<(), Infallible> { Ok(()) }
//! }
//! impl Refinement<u32> for Gid {
//!     type Error = Infallible;
//!     fn check(_: &u32) -> Result<(), Infallible> { Ok(()) }
//! }
//! let uid: Refined<u32, Uid> = Refined::parse(1000).unwrap();
//! let _same: Refined<u32, Uid> = uid;
//! ```
//!
//! A gid is never a uid, although the predicates are equal:
//!
//! ```compile_fail,E0308
//! # use std::convert::Infallible;
//! # use amoeboid::refined::{Refined, Refinement};
//! # struct Uid;
//! # struct Gid;
//! # impl Refinement<u32> for Uid {
//! #     type Error = Infallible;
//! #     fn check(_: &u32) -> Result<(), Infallible> { Ok(()) }
//! # }
//! # impl Refinement<u32> for Gid {
//! #     type Error = Infallible;
//! #     fn check(_: &u32) -> Result<(), Infallible> { Ok(()) }
//! # }
//! let uid: Refined<u32, Uid> = Refined::parse(1000).unwrap();
//! let _gid: Refined<u32, Gid> = uid;
//! ```
//!
//! There is no `From<T>`:
//!
//! ```compile_fail,E0277
//! # use std::convert::Infallible;
//! # use amoeboid::refined::{Refined, Refinement};
//! # struct Uid;
//! # impl Refinement<u32> for Uid {
//! #     type Error = Infallible;
//! #     fn check(_: &u32) -> Result<(), Infallible> { Ok(()) }
//! # }
//! let _uid: Refined<u32, Uid> = 1000.into();
//! ```
//!
//! # Constants
//!
//! Traits cannot have `const` methods on stable, so a marker that wants literals repeats its
//! predicate as an inherent `const fn check_const` (the contract is in [`Literal`]); a test per
//! marker compares it with `check` over the predicate's edge table. [`refined!`] evaluates it in
//! a `const` block, so an invalid constant does not compile:
//!
//! ```
//! use amoeboid::domain::Seconds;
//! use amoeboid::refined;
//!
//! const BOOT: Seconds = refined!(Seconds, 180);
//! assert_eq!(*BOOT.get(), 180);
//! ```
//!
//! ```compile_fail,E0080
//! # use amoeboid::domain::Seconds;
//! # use amoeboid::refined;
//! const NEVER: Seconds = refined!(Seconds, 0);
//! ```
//!
//! String-based types take string literals the same way:
//!
//! ```
//! use amoeboid::domain::PresetName;
//! use amoeboid::refined;
//!
//! let name: PresetName = refined!(PresetName, "default");
//! assert_eq!(name.get(), "default");
//! ```
//!
//! ```compile_fail,E0080
//! # use amoeboid::domain::PresetName;
//! # use amoeboid::refined;
//! let name: PresetName = refined!(PresetName, "../etc");
//! ```
//!
//! # No unchecked constructor
//!
//! A [`Checked`] or [`CheckedStr`] is the only way to a [`Refined`] besides `parse`, and only this
//! crate can make one (inside its markers' `check_const`). Outside the crate a value cannot be
//! vouched for, so every [`Refined`] there went through its predicate:
//!
//! ```compile_fail,E0624
//! use amoeboid::domain::seconds::SecondsRange;
//! use amoeboid::refined::{Checked, Refined};
//!
//! let forged: Refined<u32, SecondsRange> = Checked::new(0).refine();
//! ```
//!
//! ```compile_fail,E0624
//! use amoeboid::domain::preset_name::PresetNameGrammar;
//! use amoeboid::refined::{CheckedStr, Refined};
//!
//! let forged: Refined<String, PresetNameGrammar> = CheckedStr::new("../x").refine();
//! ```

use std::cmp::Ordering;
use std::convert::Infallible;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;

use serde::de::{self, Deserialize, Deserializer};
use serde::{Serialize, Serializer};

use crate::diagnostic::Diagnostic;

/// A predicate over `T` with its own exhaustive error type.
pub trait Refinement<T> {
    type Error: Diagnostic + Eq + fmt::Debug;

    /// Succeeds exactly when `value` satisfies the predicate.
    ///
    /// # Errors
    /// The variant of [`Self::Error`] naming the violated edge of the predicate.
    fn check(value: &T) -> Result<(), Self::Error>;
}

/// `T` proven to satisfy `P`. The field is private; the ways in are [`Refined::parse`],
/// [`Deserialize`] and [`refined!`], all of which run the predicate (the last at compile time).
pub struct Refined<T, P: Refinement<T>>(T, PhantomData<fn() -> P>);

impl<T, P: Refinement<T>> Refined<T, P> {
    /// # Errors
    /// The predicate's error when `value` does not satisfy `P`.
    pub fn parse(value: T) -> Result<Self, P::Error> {
        P::check(&value)?;
        Ok(Self(value, PhantomData))
    }

    #[must_use]
    pub const fn get(&self) -> &T {
        &self.0
    }

    #[must_use]
    pub fn into_inner(self) -> T {
        self.0
    }
}

/// A value that a marker's `check_const` has vouched for; [`Checked::refine`] turns it into the
/// [`Refined`] value. Constructible only inside this crate (field and constructor are private to
/// it).
pub struct Checked<T, P: Refinement<T>>(T, PhantomData<fn() -> P>);

impl<T, P: Refinement<T>> Checked<T, P> {
    /// Vouches that `value` satisfies `P`. Crate-private, so no caller outside this crate can
    /// forge a proof; call it only inside a marker's `check_const`, on the branch where the
    /// predicate holds, and use [`Refined::parse`] everywhere else.
    #[must_use]
    pub(crate) const fn new(value: T) -> Self {
        Self(value, PhantomData)
    }
}

impl<T: Copy, P: Refinement<T>> Checked<T, P> {
    /// Completes a compile-time check made by the marker's `check_const`. Public because
    /// [`refined!`] expands in the caller's crate; sound because only this crate can make a
    /// [`Checked`]. `T: Copy` because a generic destructor cannot run in a `const` context;
    /// such constants are integers.
    #[must_use]
    pub const fn refine(self) -> Refined<T, P> {
        Refined(self.0, PhantomData)
    }
}

impl<T: Copy, P: Refinement<T>> Clone for Checked<T, P> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: Copy, P: Refinement<T>> Copy for Checked<T, P> {}

/// A string literal that the `check_const` of a marker over `String` has vouched for;
/// [`CheckedStr::refine`] copies it into the [`Refined`] value. Like [`Checked`], only this crate
/// can make one.
pub struct CheckedStr<P: Refinement<String>>(&'static str, PhantomData<fn() -> P>);

impl<P: Refinement<String>> CheckedStr<P> {
    /// Vouches that `value` satisfies `P`; the same contract as [`Checked::new`].
    #[must_use]
    pub(crate) const fn new(value: &'static str) -> Self {
        Self(value, PhantomData)
    }

    /// The owned refined string. Not `const` (it allocates); the check itself ran at compile time.
    #[must_use]
    pub fn refine(self) -> Refined<String, P> {
        Refined(self.0.to_owned(), PhantomData)
    }
}

impl<P: Refinement<String>> Clone for CheckedStr<P> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<P: Refinement<String>> Copy for CheckedStr<P> {}

/// What [`refined!`] needs from a type to build a constant of it; implemented by every
/// [`Refined`]. The marker must provide
/// `const fn check_const(value: Value) -> Option<Checked<Value, Marker>>` (or, for a `String`
/// base, `const fn check_const(value: &'static str) -> Option<CheckedStr<Marker>>`), returning
/// `Some` exactly when [`Refinement::check`] would succeed.
pub trait Literal {
    type Value;
    type Marker;
}

impl<T, P: Refinement<T>> Literal for Refined<T, P> {
    type Value = T;
    type Marker = P;
}

/// `refined!(Type, literal)`: a value of the refined `Type`, checked at compile time.
///
/// The marker's `check_const` runs inside a `const` block; a literal that fails the predicate is a
/// compile error, so the `panic!` below can never run. For integer bases the whole expression is
/// `const`; for `String` bases the checked literal is copied into a `String` at run time.
#[macro_export]
macro_rules! refined {
    ($ty:ty, $lit:expr) => {
        const {
            match <<$ty as $crate::refined::Literal>::Marker>::check_const($lit) {
                Some(checked) => checked,
                #[allow(
                    clippy::panic,
                    reason = "evaluated at compile time: an invalid constant does not compile"
                )]
                None => panic!(concat!(
                    "invalid refined constant: ",
                    stringify!($ty),
                    " = ",
                    stringify!($lit)
                )),
            }
        }
        .refine()
    };
}

// Hand-written so that the marker needs no derives of its own.
impl<T: Clone, P: Refinement<T>> Clone for Refined<T, P> {
    fn clone(&self) -> Self {
        Self(self.0.clone(), PhantomData)
    }
}

impl<T: Copy, P: Refinement<T>> Copy for Refined<T, P> {}

impl<T: PartialEq, P: Refinement<T>> PartialEq for Refined<T, P> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl<T: Eq, P: Refinement<T>> Eq for Refined<T, P> {}

impl<T: PartialOrd, P: Refinement<T>> PartialOrd for Refined<T, P> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.0.partial_cmp(&other.0)
    }
}

impl<T: Ord, P: Refinement<T>> Ord for Refined<T, P> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.cmp(&other.0)
    }
}

impl<T: Hash, P: Refinement<T>> Hash for Refined<T, P> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

/// Shows the inner value only: the marker is in the type, not in the data.
impl<T: fmt::Debug, P: Refinement<T>> fmt::Debug for Refined<T, P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl<T: fmt::Display, P: Refinement<T>> fmt::Display for Refined<T, P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl<T, P: Refinement<T>> AsRef<T> for Refined<T, P> {
    fn as_ref(&self) -> &T {
        &self.0
    }
}

impl<T: Serialize, P: Refinement<T>> Serialize for Refined<T, P> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

/// Decodes `T`, then runs the predicate: JSON at a boundary decodes straight into proofs, and a
/// violation is reported with the predicate's diagnostic message.
impl<'de, T: Deserialize<'de>, P: Refinement<T>> Deserialize<'de> for Refined<T, P> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = T::deserialize(deserializer)?;
        Self::parse(value).map_err(|error| de::Error::custom(error.message()))
    }
}

/// For predicates that cannot fail (and for documentation examples).
impl Diagnostic for Infallible {
    fn code(&self) -> &'static str {
        match *self {}
    }

    fn message(&self) -> String {
        match *self {}
    }

    fn hint(&self) -> Option<String> {
        match *self {}
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::collections::BTreeSet;
    use std::collections::hash_map::DefaultHasher;

    use super::*;
    use crate::testing;

    #[derive(Debug, PartialEq, Eq)]
    enum PercentError {
        Zero,
        TooLarge { value: u32 },
    }

    impl Diagnostic for PercentError {
        fn code(&self) -> &'static str {
            match self {
                Self::Zero => "percent.zero",
                Self::TooLarge { .. } => "percent.too-large",
            }
        }

        fn message(&self) -> String {
            match self {
                Self::Zero => "percentage is zero".to_owned(),
                Self::TooLarge { value } => format!("percentage {value} is above 100"),
            }
        }

        fn hint(&self) -> Option<String> {
            match self {
                Self::Zero => Some("use at least 1".to_owned()),
                Self::TooLarge { .. } => None,
            }
        }
    }

    /// `1..=100`.
    struct Range;
    /// Accepts everything: a distinct marker over the same base type.
    struct OtherRange;

    impl Refinement<u32> for Range {
        type Error = PercentError;

        fn check(value: &u32) -> Result<(), PercentError> {
            match *value {
                0 => Err(PercentError::Zero),
                1..=100 => Ok(()),
                value @ 101.. => Err(PercentError::TooLarge { value }),
            }
        }
    }

    impl Range {
        const fn check_const(value: u32) -> Option<Checked<u32, Range>> {
            if matches!(value, 1..=100) {
                Some(Checked::new(value))
            } else {
                None
            }
        }
    }

    impl Refinement<u32> for OtherRange {
        type Error = Infallible;

        fn check(_: &u32) -> Result<(), Infallible> {
            Ok(())
        }
    }

    type Percent = Refined<u32, Range>;

    #[test]
    fn parse_accepts_values_inside_the_predicate_and_refuses_each_edge() {
        testing::table(
            &[
                (0, Err(PercentError::Zero)),
                (1, Ok(1)),
                (2, Ok(2)),
                (99, Ok(99)),
                (100, Ok(100)),
                (101, Err(PercentError::TooLarge { value: 101 })),
                (u32::MAX, Err(PercentError::TooLarge { value: u32::MAX })),
            ],
            |value| Percent::parse(*value).map(Percent::into_inner),
        );
    }

    #[test]
    fn accepted_values_parse_back_to_themselves() {
        for raw in [1, 50, 100] {
            let percent = Percent::parse(raw).unwrap();
            assert_eq!(Percent::parse(*percent.get()), Ok(percent));
        }
    }

    #[test]
    fn get_as_ref_and_into_inner_expose_the_value_read_only() {
        let percent = Percent::parse(42).unwrap();
        assert_eq!(*percent.get(), 42);
        assert_eq!(*percent.as_ref(), 42);
        assert_eq!(percent.into_inner(), 42);
    }

    #[test]
    fn refined_values_compare_order_hash_and_format_like_their_inner_value() {
        let (low, high) = (Percent::parse(5).unwrap(), Percent::parse(7).unwrap());
        assert_eq!(low.clone(), low);
        assert_ne!(low, high);
        assert!(low < high);
        assert_eq!(low.cmp(&high), Ordering::Less);
        assert_eq!(low.partial_cmp(&high), Some(Ordering::Less));
        assert_eq!(format!("{low:?} {low}"), "5 5");
        let hash = |p: &Percent| {
            let mut hasher = DefaultHasher::new();
            p.hash(&mut hasher);
            hasher.finish()
        };
        assert_eq!(hash(&low), hash(&Percent::parse(5).unwrap()));
        let set: BTreeSet<Percent> = [high, low, low].into_iter().collect();
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn markers_with_equal_base_types_are_judged_by_their_own_predicate() {
        assert_eq!(
            Refined::<u32, OtherRange>::parse(0).map(Refined::into_inner),
            Ok(0)
        );
        assert_eq!(Percent::parse(0), Err(PercentError::Zero));
    }

    #[test]
    fn refined_macro_builds_constants_that_satisfy_the_predicate() {
        const ONE: Percent = refined!(Percent, 1);
        const HUNDRED: Percent = refined!(Percent, 100);
        assert_eq!(Percent::parse(1), Ok(ONE));
        assert_eq!(Percent::parse(100), Ok(HUNDRED));
    }

    /// Non-empty text.
    struct Word;

    impl Refinement<String> for Word {
        type Error = PercentError;

        fn check(value: &String) -> Result<(), PercentError> {
            if value.is_empty() {
                Err(PercentError::Zero)
            } else {
                Ok(())
            }
        }
    }

    impl Word {
        const fn check_const(value: &'static str) -> Option<CheckedStr<Word>> {
            if value.is_empty() {
                None
            } else {
                Some(CheckedStr::new(value))
            }
        }
    }

    #[test]
    fn refined_macro_builds_string_values_that_satisfy_the_predicate() {
        let word: Refined<String, Word> = refined!(Refined<String, Word>, "x");
        assert_eq!(Refined::<String, Word>::parse("x".to_owned()), Ok(word));
    }

    #[test]
    fn check_const_agrees_with_check_at_every_edge() {
        for value in [0, 1, 2, 99, 100, 101, u32::MAX] {
            assert_eq!(
                Range::check_const(value).is_some(),
                Range::check(&value).is_ok(),
                "value {value}"
            );
        }
    }

    #[test]
    fn json_decodes_straight_into_refined_values() {
        // serde_json appends " at line L column C" to custom messages.
        fn message(text: &str) -> Result<u32, String> {
            serde_json::from_str::<Percent>(text)
                .map(Percent::into_inner)
                .map_err(|error| {
                    let text = error.to_string();
                    let cut = text.find(" at line").unwrap_or(text.len());
                    text.get(..cut).unwrap_or_default().to_owned()
                })
        }
        testing::table(
            &[
                ("50", Ok(50)),
                ("100", Ok(100)),
                ("0", Err("percentage is zero".to_owned())),
                ("101", Err("percentage 101 is above 100".to_owned())),
                (
                    "\"x\"",
                    Err("invalid type: string \"x\", expected u32".to_owned()),
                ),
            ],
            |text| message(text),
        );
    }

    #[test]
    fn refined_values_serialize_as_their_inner_value() {
        let percent = Percent::parse(30).unwrap();
        assert_eq!(serde_json::to_string(&percent).unwrap(), "30");
    }
}
