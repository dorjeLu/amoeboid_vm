"""KdlSerde is a bijection between `Value` trees and canonical KDL text."""

import math
from typing import cast

import pytest
from hypothesis import given
from hypothesis import strategies as st

from amoeboid.kdl_serde import Document, KdlSerde, Value
from amoeboid.result import Err, Ok

# -- strict structural equality --------------------------------------------------------
# `==` is too lenient for a bijection: True == 1, 0.0 == -0.0, and NaN != NaN.


def same(a: Value, b: Value) -> bool:
    if type(a) is not type(b):
        return False
    match a:
        case dict():
            bd = cast(dict[str, Value], b)
            return list(a) == list(bd) and all(same(a[k], bd[k]) for k in a)
        case list():
            bl = cast(list[Value], b)
            return len(a) == len(bl) and all(same(x, y) for x, y in zip(a, bl, strict=True))
        case float():
            bf = cast(float, b)
            if math.isnan(a):
                return math.isnan(bf)
            return a == bf and math.copysign(1.0, a) == math.copysign(1.0, bf)
        case _:
            return a == b


# -- generators -------------------------------------------------------------------------

texts = st.text(st.characters(exclude_categories=["Cs"]))  # anything but lone surrogates
scalars: st.SearchStrategy[Value] = st.one_of(
    st.none(),
    st.booleans(),
    st.integers(),
    st.integers(min_value=-(2**200), max_value=2**200),
    st.floats(allow_nan=True, allow_infinity=True),
    texts,
)
values: st.SearchStrategy[Value] = st.recursive(
    scalars,
    lambda inner: st.one_of(
        st.lists(inner, max_size=5),
        st.dictionaries(texts, inner, max_size=5),
    ),
    max_leaves=30,
)
documents: st.SearchStrategy[Document] = st.dictionaries(texts, values, max_size=6)


def dumps(doc: Document) -> str:
    match KdlSerde.dumps(doc):
        case Ok(text):
            return text
        case Err(error):
            pytest.fail(error)


def loads(text: str) -> Document:
    match KdlSerde.loads(text):
        case Ok(doc):
            return doc
        case Err(error):
            pytest.fail(f"{error}\n--- text ---\n{text}")


# -- the bijection ---------------------------------------------------------------------


@given(documents)
def test_python_to_kdl_to_python_is_identity(doc: Document) -> None:
    assert same(loads(dumps(doc)), doc)


@given(documents)
def test_kdl_to_python_to_kdl_is_identity_on_canonical_text(doc: Document) -> None:
    text = dumps(doc)
    assert dumps(loads(text)) == text


@given(documents, documents)
def test_distinct_documents_render_distinctly(a: Document, b: Document) -> None:
    if not same(a, b):
        assert dumps(a) != dumps(b)


# -- the canonical form, by example ------------------------------------------------------

EXAMPLE: Document = {
    "user": "alice",
    "uid": 1000,
    "ratio": -0.0,
    "on": True,
    "nothing": None,
    "ssh": {"port": 0},
    "empty-dict": {},
    "empty-list": [],
    "mounts": [{"path": "/home/alice/dev", "mode": "ro"}, ["nested", 1.5]],
    "true": "keyword-looking key",
    "has space": "‎tab\there",
    "-": {"-": "dash key inside a dict"},
}

EXAMPLE_KDL = """\
user "alice"
uid 1000
ratio -0.0
on #true
nothing #null
ssh {
    port 0
}
empty-dict {}
(list)empty-list {}
(list)mounts {
    - {
        path "/home/alice/dev"
        mode "ro"
    }
    (list)- {
        - "nested"
        - 1.5
    }
}
"true" "keyword-looking key"
"has space" "\\u{200e}tab\\there"
- {
    - "dash key inside a dict"
}
"""


def test_example_renders_canonically() -> None:
    assert dumps(EXAMPLE) == EXAMPLE_KDL


def test_example_parses_back() -> None:
    assert same(loads(EXAMPLE_KDL), EXAMPLE)


def test_non_canonical_layout_and_comments_are_accepted() -> None:
    text = 'ssh { port 22; } // trailing comment\n/- dropped "node"\n(list)xs { - 1; - 2 }'
    assert same(loads(text), {"ssh": {"port": 22}, "xs": [1, 2]})


@pytest.mark.parametrize(
    ("text", "complaint"),
    [
        ("a 1 2", "expected one value, got 2"),
        ("a x=1", "properties (x) aren't supported"),
        ("a 1 { b 2 }", "either one value or children"),
        ("a 1\na 2", "duplicate key 'a'"),
        ("(u8)a 1", "unknown type annotation (u8)"),
        ("a (u8)1", "type-annotated values"),
        ("(list)a { x 1 }", "list items must be named '-'"),
        ("(list)a 1", "takes children, not arguments"),
        ("a true", "invalid KDL"),
    ],
)
def test_non_canonical_shapes_are_rejected(text: str, complaint: str) -> None:
    match KdlSerde.loads(text):
        case Err(error):
            assert complaint in error
        case Ok(doc):
            pytest.fail(f"accepted {text!r} as {doc!r}")


def test_lone_surrogates_are_reported_not_raised() -> None:
    assert KdlSerde.dumps({"a": "\ud800"}) == Err("lone surrogate U+D800 can't be written as KDL")


def test_is_a_namespace() -> None:
    with pytest.raises(TypeError):
        KdlSerde()
