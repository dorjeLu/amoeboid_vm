//! Railway building blocks (design §Errors): non-empty lists, error-collecting validation and
//! transient-fault retry. Nothing here performs effects except through the [`Clock`] it is given.

use std::future::Future;
use std::num::{NonZeroU8, NonZeroUsize};
use std::time::Duration;

/// A list with at least one element; the only way to carry "one or more problems".
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct NonEmpty<T> {
    head: T,
    tail: Vec<T>,
}

impl<T> NonEmpty<T> {
    pub fn new(head: T, tail: Vec<T>) -> Self {
        Self { head, tail }
    }

    pub fn singleton(head: T) -> Self {
        Self::new(head, Vec::new())
    }

    /// `None` for an empty vector.
    #[must_use]
    pub fn from_vec(items: Vec<T>) -> Option<Self> {
        let mut items = items.into_iter();
        let head = items.next()?;
        Some(Self::new(head, items.collect()))
    }

    pub fn first(&self) -> &T {
        &self.head
    }

    pub fn len(&self) -> NonZeroUsize {
        NonZeroUsize::MIN
            .checked_add(self.tail.len())
            .unwrap_or(NonZeroUsize::MAX)
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> {
        std::iter::once(&self.head).chain(&self.tail)
    }

    pub fn push(&mut self, item: T) {
        self.tail.push(item);
    }

    /// Appends all of `other` after `self`, keeping order.
    #[must_use]
    pub fn append(mut self, other: Self) -> Self {
        self.tail.push(other.head);
        self.tail.extend(other.tail);
        self
    }

    pub fn map<U>(self, mut f: impl FnMut(T) -> U) -> NonEmpty<U> {
        NonEmpty {
            head: f(self.head),
            tail: self.tail.into_iter().map(f).collect(),
        }
    }

    pub fn into_vec(self) -> Vec<T> {
        let mut items = Vec::with_capacity(self.tail.len().saturating_add(1));
        items.push(self.head);
        items.extend(self.tail);
        items
    }
}

impl<T> IntoIterator for NonEmpty<T> {
    type Item = T;
    type IntoIter = std::iter::Chain<std::iter::Once<T>, std::vec::IntoIter<T>>;

    fn into_iter(self) -> Self::IntoIter {
        std::iter::once(self.head).chain(self.tail)
    }
}

/// Applicative validation: combining validations keeps the errors of every operand, in order,
/// where `Result` and `?` stop at the first.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Validated<T, E> {
    Valid(T),
    Invalid(NonEmpty<E>),
}

impl<T, E> Validated<T, E> {
    pub fn invalid(error: E) -> Self {
        Self::Invalid(NonEmpty::singleton(error))
    }

    pub fn from_result(result: Result<T, E>) -> Self {
        match result {
            Ok(value) => Self::Valid(value),
            Err(error) => Self::invalid(error),
        }
    }

    /// # Errors
    ///
    /// The collected errors of an invalid value.
    pub fn into_result(self) -> Result<T, NonEmpty<E>> {
        match self {
            Self::Valid(value) => Ok(value),
            Self::Invalid(errors) => Err(errors),
        }
    }

    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Validated<U, E> {
        match self {
            Self::Valid(value) => Validated::Valid(f(value)),
            Self::Invalid(errors) => Validated::Invalid(errors),
        }
    }

    /// Both values, or the errors of `self` followed by the errors of `other`.
    pub fn zip<U>(self, other: Validated<U, E>) -> Validated<(T, U), E> {
        match (self, other) {
            (Self::Valid(a), Validated::Valid(b)) => Validated::Valid((a, b)),
            (Self::Invalid(errors), Validated::Valid(_))
            | (Self::Valid(_), Validated::Invalid(errors)) => Validated::Invalid(errors),
            (Self::Invalid(first), Validated::Invalid(second)) => {
                Validated::Invalid(first.append(second))
            }
        }
    }

    /// All values in order, or every error of every item in order.
    pub fn all(items: impl IntoIterator<Item = Self>) -> Validated<Vec<T>, E> {
        let mut values = Vec::new();
        let mut errors = Vec::new();
        for item in items {
            match item {
                Self::Valid(value) => values.push(value),
                Self::Invalid(found) => errors.extend(found),
            }
        }
        match NonEmpty::from_vec(errors) {
            Some(errors) => Validated::Invalid(errors),
            None => Validated::Valid(values),
        }
    }
}

/// A failed attempt of an operation that can fail transiently: the caller may try again
/// (`Transient`) or must not (`Permanent`). Only such operations return it, and [`retry`]
/// accepts nothing else.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Fault<T, P> {
    Transient(T),
    Permanent(P),
}

/// How [`retry`] ended without a value.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Retry<T, P> {
    /// Every attempt failed transiently; `last` is the final failure.
    Exhausted {
        last: T,
        attempts: NonZeroU8,
    },
    Permanent(P),
}

/// Exponential backoff: the delay after failed attempt `n` is `initial * 2^(n-1)`, at most `cap`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Backoff {
    initial: Duration,
    cap: Duration,
    max_attempts: NonZeroU8,
}

impl Backoff {
    /// A `cap` below `initial` is raised to `initial`, so every delay lies in `initial..=cap`.
    #[must_use]
    pub fn new(initial: Duration, cap: Duration, max_attempts: NonZeroU8) -> Self {
        Self {
            initial,
            cap: cap.max(initial),
            max_attempts,
        }
    }

    #[must_use]
    pub fn max_attempts(&self) -> NonZeroU8 {
        self.max_attempts
    }

    /// The pause after failed attempt number `attempt` (counting from 1).
    #[must_use]
    pub fn delay(&self, attempt: NonZeroU8) -> Duration {
        let doublings = attempt.get().saturating_sub(1);
        let factor = 1_u32.checked_shl(u32::from(doublings)).unwrap_or(u32::MAX);
        self.initial.saturating_mul(factor).min(self.cap)
    }
}

/// Sleeping, as far as [`retry`] needs it; the `Clock` port in `ports` extends this with time
/// reads, and tests substitute a recording fake.
pub trait Clock {
    fn sleep(&self, duration: Duration) -> impl Future<Output = ()> + Send;
}

/// Runs `op` until it succeeds, fails permanently or `policy.max_attempts()` attempts have
/// failed transiently, pausing `policy.delay(n)` on `clock` after failed attempt `n`.
///
/// # Errors
///
/// [`Retry::Permanent`] with the first permanent failure, or [`Retry::Exhausted`] with the last
/// transient one.
pub async fn retry<A, T, P, F, Fut>(
    clock: &impl Clock,
    policy: Backoff,
    mut op: F,
) -> Result<A, Retry<T, P>>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<A, Fault<T, P>>>,
{
    let mut attempt = NonZeroU8::MIN;
    loop {
        match op().await {
            Ok(value) => return Ok(value),
            Err(Fault::Permanent(error)) => return Err(Retry::Permanent(error)),
            Err(Fault::Transient(last)) => {
                let next = attempt
                    .checked_add(1)
                    .filter(|_| attempt < policy.max_attempts);
                let Some(next) = next else {
                    return Err(Retry::Exhausted {
                        last,
                        attempts: attempt,
                    });
                };
                clock.sleep(policy.delay(attempt)).await;
                attempt = next;
            }
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testing::{every_variant, table};
    use std::sync::Mutex;

    fn ne(items: &[u32]) -> NonEmpty<u32> {
        NonEmpty::from_vec(items.to_vec()).unwrap()
    }

    #[test]
    fn non_empty_from_vec_refuses_empty_and_keeps_order() {
        table(
            &[
                (vec![], None),
                (vec![7], Some(NonEmpty::new(7, vec![]))),
                (vec![1, 2, 3], Some(NonEmpty::new(1, vec![2, 3]))),
            ],
            |items: &Vec<u32>| NonEmpty::from_vec(items.clone()),
        );
    }

    #[test]
    fn non_empty_accessors_agree_with_the_vector_form() {
        table(
            &[
                (vec![5], (5, 1, vec![5])),
                (vec![1, 2, 3], (1, 3, vec![1, 2, 3])),
            ],
            |items: &Vec<u32>| {
                let list = ne(items);
                let seen: Vec<u32> = list.iter().copied().collect();
                assert_eq!(list.clone().into_iter().collect::<Vec<_>>(), seen);
                assert_eq!(list.clone().into_vec(), seen);
                (*list.first(), list.len().get(), seen)
            },
        );
    }

    #[test]
    fn non_empty_push_append_and_map_keep_order() {
        let mut list = NonEmpty::singleton(1);
        list.push(2);
        assert_eq!(list, ne(&[1, 2]));
        assert_eq!(list.append(ne(&[3, 4])), ne(&[1, 2, 3, 4]));
        assert_eq!(ne(&[1, 2, 3]).map(|n| n * 10), ne(&[10, 20, 30]));
    }

    type V = Validated<u32, &'static str>;

    fn invalid(errors: &[&'static str]) -> V {
        Validated::Invalid(NonEmpty::from_vec(errors.to_vec()).unwrap())
    }

    #[test]
    fn validated_converts_to_and_from_result() {
        assert_eq!(V::from_result(Ok(1)), Validated::Valid(1));
        assert_eq!(V::from_result(Err("bad")), invalid(&["bad"]));
        assert_eq!(V::invalid("bad"), invalid(&["bad"]));
        assert_eq!(V::Valid(1).into_result(), Ok(1));
        assert_eq!(
            invalid(&["a", "b"]).into_result(),
            Err(NonEmpty::new("a", vec!["b"]))
        );
    }

    #[test]
    fn validated_map_transforms_only_a_valid_value() {
        assert_eq!(V::Valid(2).map(|n| n + 1), Validated::Valid(3));
        assert_eq!(invalid(&["a"]).map(|n| n + 1), invalid(&["a"]));
    }

    #[test]
    fn validated_zip_collects_every_error_in_order() {
        let cases: [((V, V), Pair); 4] = [
            ((V::Valid(1), V::Valid(2)), Validated::Valid((1, 2))),
            (
                (invalid(&["a", "b"]), V::Valid(2)),
                invalid_pair(&["a", "b"]),
            ),
            ((V::Valid(1), invalid(&["c"])), invalid_pair(&["c"])),
            (
                (invalid(&["a", "b"]), invalid(&["c", "d"])),
                invalid_pair(&["a", "b", "c", "d"]),
            ),
        ];
        table(&cases, |(left, right)| left.clone().zip(right.clone()));
    }

    type Pair = Validated<(u32, u32), &'static str>;

    fn invalid_pair(errors: &[&'static str]) -> Pair {
        Validated::Invalid(NonEmpty::from_vec(errors.to_vec()).unwrap())
    }

    #[test]
    fn validated_all_collects_values_or_every_error_in_order() {
        type Collected = Validated<Vec<u32>, &'static str>;
        let cases: [(Vec<V>, Collected); 5] = [
            (vec![], Validated::Valid(vec![])),
            (vec![V::Valid(1), V::Valid(2)], Validated::Valid(vec![1, 2])),
            (
                vec![V::Valid(1), invalid(&["a"]), V::Valid(2)],
                Validated::invalid("a"),
            ),
            (
                vec![invalid(&["a", "b"]), V::Valid(1), invalid(&["c"])],
                Validated::Invalid(NonEmpty::new("a", vec!["b", "c"])),
            ),
            (
                vec![invalid(&["x"]), invalid(&["x"])],
                Validated::Invalid(NonEmpty::new("x", vec!["x"])),
            ),
        ];
        table(&cases, |items| Validated::all(items.clone()));
    }

    #[derive(Debug)]
    struct FakeClock(Mutex<Vec<Duration>>);

    impl FakeClock {
        fn new() -> Self {
            Self(Mutex::new(Vec::new()))
        }

        fn sleeps(&self) -> Vec<Duration> {
            self.0.lock().unwrap().clone()
        }
    }

    impl Clock for FakeClock {
        fn sleep(&self, duration: Duration) -> impl Future<Output = ()> + Send {
            self.0.lock().unwrap().push(duration);
            std::future::ready(())
        }
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn attempts(n: u8) -> NonZeroU8 {
        NonZeroU8::new(n).unwrap()
    }

    fn policy(max: u8) -> Backoff {
        Backoff::new(ms(10), ms(100), attempts(max))
    }

    #[test]
    fn backoff_delays_double_until_the_cap() {
        let backoff = policy(255);
        let delays: Vec<u64> = [1, 2, 3, 4, 5, 6, 7, 32, 33, 255]
            .into_iter()
            .map(|n| u64::try_from(backoff.delay(attempts(n)).as_millis()).unwrap())
            .collect();
        assert_eq!(delays, [10, 20, 40, 80, 100, 100, 100, 100, 100, 100]);
    }

    #[test]
    fn backoff_cap_below_initial_is_raised_to_initial() {
        let backoff = Backoff::new(ms(10), ms(1), attempts(3));
        assert_eq!(backoff, Backoff::new(ms(10), ms(10), attempts(3)));
        assert_eq!(backoff.delay(attempts(3)), ms(10));
        assert_eq!(backoff.max_attempts(), attempts(3));
    }

    #[test]
    fn backoff_with_a_huge_initial_delay_saturates_at_the_cap() {
        let backoff = Backoff::new(Duration::MAX, Duration::MAX, attempts(255));
        assert_eq!(backoff.delay(attempts(255)), Duration::MAX);
    }

    type Outcome = Result<&'static str, Fault<u8, &'static str>>;

    /// Runs `retry` with an op that returns `script(call)` for call number `call` (from 1).
    fn run(
        max: u8,
        script: impl Fn(u8) -> Outcome,
    ) -> (
        Result<&'static str, Retry<u8, &'static str>>,
        u8,
        Vec<Duration>,
    ) {
        let clock = FakeClock::new();
        let calls = Mutex::new(0_u8);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let result = runtime.block_on(retry(&clock, policy(max), || {
            let mut calls = calls.lock().unwrap();
            *calls = calls.saturating_add(1);
            std::future::ready(script(*calls))
        }));
        let calls = *calls.lock().unwrap();
        (result, calls, clock.sleeps())
    }

    fn assert_pauses_are_the_policy(max: u8, calls: u8, sleeps: &[Duration]) {
        let backoff = policy(max);
        let expected: Vec<Duration> = (1..calls).map(|n| backoff.delay(attempts(n))).collect();
        assert_eq!(sleeps, expected, "max {max}");
        assert!(sleeps.is_sorted(), "non-decreasing: {sleeps:?}");
        assert!(sleeps.iter().all(|d| *d <= ms(100)), "capped: {sleeps:?}");
    }

    #[test]
    fn retry_stops_after_max_attempts_of_transient_failures() {
        for max in 1..=u8::MAX {
            let (result, calls, sleeps) = run(max, |n| Err(Fault::Transient(n)));
            assert_eq!(
                result,
                Err(Retry::Exhausted {
                    last: max,
                    attempts: attempts(max)
                })
            );
            assert_eq!(calls, max);
            assert_eq!(
                sleeps.len(),
                usize::from(max) - 1,
                "no pause after the last"
            );
            assert_pauses_are_the_policy(max, calls, &sleeps);
        }
    }

    #[test]
    fn retry_stops_at_the_first_permanent_failure_without_sleeping_after_it() {
        for max in 1..=8 {
            for at in 1..=max {
                let (result, calls, sleeps) = run(max, |n| {
                    if n < at {
                        Err(Fault::Transient(n))
                    } else {
                        Err(Fault::Permanent("no"))
                    }
                });
                assert_eq!(result, Err(Retry::Permanent("no")), "max {max} at {at}");
                assert_eq!(calls, at);
                assert_pauses_are_the_policy(max, calls, &sleeps);
            }
        }
    }

    #[test]
    fn retry_returns_the_first_success_without_further_calls() {
        for max in 1..=8 {
            for at in 1..=max {
                let (result, calls, sleeps) = run(max, |n| {
                    if n < at {
                        Err(Fault::Transient(n))
                    } else {
                        Ok("done")
                    }
                });
                assert_eq!(result, Ok("done"), "max {max} at {at}");
                assert_eq!(calls, at);
                assert_pauses_are_the_policy(max, calls, &sleeps);
            }
        }
    }

    #[test]
    fn retry_with_one_attempt_never_sleeps() {
        let (result, calls, sleeps) = run(1, |n| Err(Fault::Transient(n)));
        assert_eq!(
            result,
            Err(Retry::Exhausted {
                last: 1,
                attempts: NonZeroU8::MIN
            })
        );
        assert_eq!((calls, sleeps), (1, vec![]));
    }

    fn witness_fault(e: &Fault<u8, u8>) -> &'static str {
        match e {
            Fault::Transient(_) => "Transient",
            Fault::Permanent(_) => "Permanent",
        }
    }

    fn witness_retry(e: &Retry<u8, u8>) -> &'static str {
        match e {
            Retry::Exhausted { .. } => "Exhausted",
            Retry::Permanent(_) => "Permanent",
        }
    }

    fn witness_validated(e: &Validated<u8, u8>) -> &'static str {
        match e {
            Validated::Valid(_) => "Valid",
            Validated::Invalid(_) => "Invalid",
        }
    }

    #[test]
    fn every_enum_variant_has_a_case() {
        every_variant(
            &[Fault::Transient(1), Fault::Permanent(2)],
            witness_fault,
            &["Transient", "Permanent"],
        );
        every_variant(
            &[
                Retry::Exhausted {
                    last: 1,
                    attempts: NonZeroU8::MIN,
                },
                Retry::Permanent(2),
            ],
            witness_retry,
            &["Exhausted", "Permanent"],
        );
        every_variant(
            &[Validated::Valid(1), Validated::invalid(2)],
            witness_validated,
            &["Valid", "Invalid"],
        );
    }
}
