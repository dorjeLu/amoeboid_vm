from amoeboid.result import (
    Err,
    Ok,
    Result,
    attempt,
    ensure,
    railway,
    traverse,
    traverse_all,
)


def half(n: int) -> Result[int, str]:
    return Ok(n // 2) if n % 2 == 0 else Err(f"{n} is odd")


def test_map_and_bind_follow_the_ok_track() -> None:
    assert Ok(8).bind(half).bind(half).map(str) == Ok("2")


def test_bind_stops_at_first_err() -> None:
    assert Ok(6).bind(half).bind(half).map(str) == Err("3 is odd")


def test_map_err_only_touches_errors() -> None:
    assert Ok(1).map_err(len) == Ok(1)
    assert Err("abc").map_err(len) == Err(3)


def test_railway_short_circuits_on_or_bail() -> None:
    seen: list[int] = []

    @railway
    def quarter(n: int) -> Result[int, str]:
        h = half(n).or_bail()
        seen.append(h)
        return half(h)

    assert quarter(8) == Ok(2)
    assert quarter(6) == Err("3 is odd")
    assert quarter(5) == Err("5 is odd")
    assert seen == [4, 3]


def test_attempt_converts_listed_exceptions_only() -> None:
    def boom() -> int:
        raise OSError("disk on fire")

    assert attempt(boom, str) == Err("disk on fire")
    assert attempt(lambda: 3, str) == Ok(3)


def test_traverse_variants() -> None:
    assert traverse([2, 4], half) == Ok([1, 2])
    assert traverse([2, 3, 5], half) == Err("3 is odd")
    assert traverse_all([2, 3, 5], half) == Err(["3 is odd", "5 is odd"])


def test_ensure() -> None:
    assert ensure(True, lambda: "no") == Ok(None)
    assert ensure(False, lambda: "no") == Err("no")
