"""The one error type that travels along amoeboid's rails."""

from collections.abc import Callable
from dataclasses import dataclass

from amoeboid.result import Err


@dataclass(frozen=True, slots=True)
class AppError:
    """A failure to report to the user: what went wrong, and optionally what to do."""

    message: str
    hint: str | None = None

    def render(self) -> str:
        return self.message if self.hint is None else f"{self.message}\n  hint: {self.hint}"


def fail(message: str, hint: str | None = None) -> Err[AppError]:
    return Err(AppError(message, hint))


def because(what: str, hint: str | None = None) -> Callable[[Exception], AppError]:
    """Error mapper for `attempt`: prefixes the exception text with `what`."""
    return lambda exc: AppError(f"{what}: {exc}", hint)
