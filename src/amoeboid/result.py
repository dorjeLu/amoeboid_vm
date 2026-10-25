"""Railway-oriented results.

Every fallible step returns `Ok(value)` or `Err(error)`. Steps compose two ways:

* combinators (`map`, `bind`, `map_err`, `traverse`) for short pipelines;
* `@railway` functions, where `result.or_bail()` unwraps an `Ok` or short-circuits the
  whole function with that `Err`, like Rust's `?` operator. This keeps longer sequences
  of effects flat instead of nesting lambdas.

Exceptions are converted at the edges with `attempt`; nothing inside the rails raises.
"""

import functools
from collections.abc import Callable, Iterable
from dataclasses import dataclass
from typing import Never, cast


@dataclass(frozen=True, slots=True)
class Ok[T]:
    value: T

    def map[U](self, f: Callable[[T], U]) -> Ok[U]:
        return Ok(f(self.value))

    def bind[U, E](self, f: Callable[[T], Result[U, E]]) -> Result[U, E]:
        return f(self.value)

    def map_err(self, f: Callable[[Never], object]) -> Ok[T]:
        return self

    def or_bail(self) -> T:
        return self.value

    def unwrap_or[U](self, default: U) -> T | U:
        return self.value


@dataclass(frozen=True, slots=True)
class Err[E]:
    error: E

    def map(self, f: Callable[[Never], object]) -> Err[E]:
        return self

    def bind(self, f: Callable[[Never], object]) -> Err[E]:
        return self

    def map_err[F](self, f: Callable[[E], F]) -> Err[F]:
        return Err(f(self.error))

    def or_bail(self) -> Never:
        raise _Bail(self.error)

    def unwrap_or[U](self, default: U) -> U:
        return default


type Result[T, E] = Ok[T] | Err[E]


class _Bail(Exception):
    """Carries an `Err` out of a `@railway` function. Never escapes one."""

    def __init__(self, error: object) -> None:
        super().__init__(error)
        self.error = error


def railway[**P, T, E](f: Callable[P, Result[T, E]]) -> Callable[P, Result[T, E]]:
    """Let `f` use `.or_bail()`: the first `Err` it hits becomes its return value.

    The error types of the results `f` bails on are not checked against `E`; keep a
    single error type per call graph (amoeboid uses `AppError` throughout).
    """

    @functools.wraps(f)
    def wrapper(*args: P.args, **kwargs: P.kwargs) -> Result[T, E]:
        try:
            return f(*args, **kwargs)
        except _Bail as bail:
            return Err(cast("E", bail.error))

    return wrapper


def attempt[T, E](
    thunk: Callable[[], T],
    on_error: Callable[[Exception], E],
    catch: tuple[type[Exception], ...] = (OSError,),
) -> Result[T, E]:
    """Run `thunk`, turning the listed exceptions into `Err(on_error(exc))`."""
    try:
        return Ok(thunk())
    except catch as exc:
        return Err(on_error(exc))


def traverse[T, U, E](xs: Iterable[T], f: Callable[[T], Result[U, E]]) -> Result[list[U], E]:
    """Apply `f` to each item, stopping at the first `Err`."""
    out: list[U] = []
    for x in xs:
        match f(x):
            case Ok(value):
                out.append(value)
            case Err() as err:
                return err
    return Ok(out)


def traverse_all[T, U, E](
    xs: Iterable[T], f: Callable[[T], Result[U, E]]
) -> Result[list[U], list[E]]:
    """Apply `f` to each item, collecting every `Err` instead of stopping at the first."""
    oks: list[U] = []
    errs: list[E] = []
    for x in xs:
        match f(x):
            case Ok(value):
                oks.append(value)
            case Err(error):
                errs.append(error)
    return Err(errs) if errs else Ok(oks)


def ensure[E](condition: bool, error: Callable[[], E]) -> Result[None, E]:
    """`Ok(None)` when `condition` holds, else `Err(error())`."""
    return Ok(None) if condition else Err(error())
