# Minimal stubs for the parts of ckdl (a compiled extension, no bundled types) amoeboid uses.
from collections.abc import Iterable, Sequence
from typing import Literal

type _Scalar = str | int | float | bool | None

class ParseError(Exception): ...

class Value:
    type_annotation: str | None
    value: _Scalar

class Node:
    type_annotation: str | None
    name: str
    args: list[_Scalar | Value]
    properties: dict[str, _Scalar | Value]
    children: list[Node]
    def __init__(
        self,
        type_annotation: str | None,
        name: str,
        args: Sequence[_Scalar | Value] = ...,
        children: Iterable[Node] = ...,
    ) -> None: ...

class Document:
    nodes: list[Node]

def parse(kdl_text: str, version: Literal[1, 2, "any"] = ...) -> Document: ...
