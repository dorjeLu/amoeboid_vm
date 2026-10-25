"""Bidirectional mapping between plain Python data and KDL v2 text.

The Python side is `Value`: None, bool, int, float, str, list of `Value`, or dict from str
to `Value`. A `Document` is the top-level dict. Each dict entry becomes a node named after
its key:

    scalar  ->  key "text" / key 42 / key 1.5 / key #true / key #null
    dict    ->  key { child-entries... }        (empty: key {})
    list    ->  (list)key { - item; - item }    (empty: (list)key {})

List items are nodes named `-` following the same rules, so lists and dicts nest freely.
The `(list)` annotation is what tells a list from a dict, so even empty ones and dicts
with a `-` key round-trip.

`dumps` always produces this canonical form, and `loads` accepts exactly it (in any
whitespace/quoting KDL allows), rejecting properties, multiple arguments, other type
annotations and duplicate keys rather than guessing. Hence for every valid `Value` tree
`loads(dumps(doc)) == Ok(doc)` and for every canonical text `dumps(loads(text)) == text`.
Comments are not preserved.

Parsing is delegated to ckdl; emitting is done here, since ckdl's emitter loses the
sign of -0.0, misprints subnormal floats and leaves strings like `true` unquoted.
ckdl's float parsing isn't correctly rounded either, so floats are written in their
shortest form only when ckdl reads that back exactly (see `_float`).
"""

import math
import re

import ckdl

from amoeboid.result import Err, Ok, Result

type Scalar = bool | int | float | str | None
type Value = Scalar | list[Value] | dict[str, Value]
type Document = dict[str, Value]

LIST_TAG = "list"
ITEM = "-"
INDENT = "    "


class KdlSerde:
    """Namespace of pure functions; holds no state and is never instantiated."""

    def __init__(self) -> None:
        raise TypeError("KdlSerde is a namespace, not a class to instantiate")

    # -- Python -> KDL ---------------------------------------------------------------

    @staticmethod
    def dumps(doc: Document) -> Result[str, str]:
        """Render `doc` as canonical KDL v2 text, or say which value can't be represented."""
        try:
            return Ok("".join(KdlSerde._entry_lines(key, value, 0) for key, value in doc.items()))
        except _Unrepresentable as exc:
            return Err(str(exc))

    @staticmethod
    def _entry_lines(name: str, value: Value, depth: int) -> str:
        pad = INDENT * depth
        head = KdlSerde.identifier(name)
        match value:
            case dict():
                return KdlSerde._block(pad, head, list(value.items()), depth)
            case list():
                items = [(ITEM, item) for item in value]
                return KdlSerde._block(pad, f"({LIST_TAG}){head}", items, depth)
            case _:
                return f"{pad}{head} {KdlSerde.scalar(value)}\n"

    @staticmethod
    def _block(pad: str, head: str, entries: list[tuple[str, Value]], depth: int) -> str:
        if not entries:
            return f"{pad}{head} {{}}\n"
        body = "".join(KdlSerde._entry_lines(k, v, depth + 1) for k, v in entries)
        return f"{pad}{head} {{\n{body}{pad}}}\n"

    @staticmethod
    def scalar(value: Scalar) -> str:
        match value:
            case None:
                return "#null"
            case bool():
                return "#true" if value else "#false"
            case int():
                return str(value)
            case float() if math.isnan(value):
                return "#nan"
            case float() if math.isinf(value):
                return "#inf" if value > 0 else "#-inf"
            case float():
                return _float(value)
            case str():
                return KdlSerde.quote(value)

    @staticmethod
    def identifier(name: str) -> str:
        """`name` bare when that is unambiguous KDL, quoted otherwise."""
        bare = name == ITEM or (_BARE.fullmatch(name) is not None and name not in _KEYWORDS)
        return name if bare else KdlSerde.quote(name)

    @staticmethod
    def quote(text: str) -> str:
        return '"' + "".join(_escape(ch) for ch in text) + '"'

    # -- KDL -> Python ---------------------------------------------------------------

    @staticmethod
    def loads(text: str) -> Result[Document, str]:
        """Parse canonical KDL v2 text back into a `Document`."""
        try:
            parsed = ckdl.parse(text, version=2)
        except ckdl.ParseError as exc:
            return Err(f"invalid KDL: {exc}")
        try:
            return Ok(KdlSerde._dict(parsed.nodes, "document"))
        except _Malformed as exc:
            return Err(str(exc))

    @staticmethod
    def _dict(nodes: list[ckdl.Node], where: str) -> dict[str, Value]:
        out: dict[str, Value] = {}
        for node in nodes:
            if node.name in out:
                raise _Malformed(f"{where}: duplicate key {node.name!r}")
            out[node.name] = KdlSerde._node(node, f"{where} > {node.name}")
        return out

    @staticmethod
    def _node(node: ckdl.Node, where: str) -> Value:
        if node.properties:
            raise _Malformed(f"{where}: properties ({', '.join(node.properties)}) aren't supported")
        match node.type_annotation:
            case None:
                pass
            case "list":
                if node.args:
                    raise _Malformed(f"{where}: a (list) node takes children, not arguments")
                return [KdlSerde._item(child, where, i) for i, child in enumerate(node.children)]
            case other:
                raise _Malformed(f"{where}: unknown type annotation ({other})")
        match node.args:
            case []:
                return KdlSerde._dict(node.children, where)
            case [arg] if not node.children:
                return KdlSerde._arg(arg, where)
            case [_]:
                raise _Malformed(f"{where}: a node has either one value or children, not both")
            case _:
                raise _Malformed(f"{where}: expected one value, got {len(node.args)}")

    @staticmethod
    def _item(node: ckdl.Node, where: str, index: int) -> Value:
        if node.name != ITEM:
            raise _Malformed(f"{where}: list items must be named '-', got {node.name!r}")
        return KdlSerde._node(node, f"{where}[{index}]")

    @staticmethod
    def _arg(arg: Scalar | ckdl.Value, where: str) -> Scalar:
        if isinstance(arg, ckdl.Value):
            raise _Malformed(f"{where}: type-annotated values aren't supported")
        return arg


class _Unrepresentable(ValueError):
    pass


class _Malformed(ValueError):
    pass


_BARE = re.compile(r"[A-Za-z_][A-Za-z0-9_-]*")
_KEYWORDS = frozenset({"true", "false", "null", "inf", "nan"})
_SIMPLE_ESCAPES = {
    "\\": "\\\\",
    '"': '\\"',
    "\n": "\\n",
    "\r": "\\r",
    "\t": "\\t",
    "\b": "\\b",
    "\f": "\\f",
}


def _float(value: float) -> str:
    """Shortest spelling ckdl parses back to exactly `value`.

    `repr` is the shortest exact decimal, but ckdl occasionally rounds it to a
    neighbouring double; 17 significant digits in exponent form has read back exactly
    for every double tried. Both always contain '.' or 'e', so they stay floats.
    """
    for spelling in (repr(value), f"{value:.16e}"):
        parsed = ckdl.parse(f"_ {spelling}", version=2).nodes[0].args[0]
        if isinstance(parsed, float) and parsed.hex() == value.hex():
            return spelling
    raise _Unrepresentable(f"float {value!r} doesn't survive a KDL round trip")


def _escape(ch: str) -> str:
    if (simple := _SIMPLE_ESCAPES.get(ch)) is not None:
        return simple
    if 0xD800 <= ord(ch) <= 0xDFFF:
        raise _Unrepresentable(f"lone surrogate U+{ord(ch):04X} can't be written as KDL")
    # Non-printables include every code point KDL forbids literally (controls,
    # direction marks, BOM) plus other invisible ones that are clearer escaped.
    return ch if ch.isprintable() else f"\\u{{{ord(ch):x}}}"
