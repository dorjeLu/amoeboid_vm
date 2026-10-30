//! The parser adapter: KDL v2 text to a `Document`, via the `kdl` crate, accepting exactly the
//! shapes the canonical emitter writes (in any layout and quoting KDL allows) and refusing
//! properties, several arguments, an argument next to children, other type annotations and
//! duplicate keys rather than guessing. Comments are not preserved.
//!
//! The crate recurses per nesting level and so does the conversion here; `nesting` refuses text
//! nested deeper than [`MAX_DEPTH`] before either runs.

use kdl::{KdlDocument, KdlEntry, KdlIdentifier, KdlNode, KdlValue};

use super::emit::{ITEM, LIST_TAG};
use super::nesting::{self, MAX_DEPTH};
use super::value::{Document, DocumentError, Float, Int, IntError, Value};
use crate::diagnostic::Diagnostic;

#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum ParseError {
    #[error("invalid KDL: {detail}")]
    Syntax { detail: String },
    #[error("{at}: duplicate key {key:?}")]
    DuplicateKey { at: String, key: String },
    #[error("{at}: properties ({}) aren't supported", names.join(", "))]
    Properties { at: String, names: Vec<String> },
    #[error("{at}: unknown type annotation ({annotation})")]
    UnknownAnnotation { at: String, annotation: String },
    #[error("{at}: a (list) node takes children, not arguments")]
    ListArguments { at: String },
    #[error("{at}: list items must be named '-', got {name:?}")]
    ListItemName { at: String, name: String },
    #[error("{at}: expected one value, got {count}")]
    ArgumentCount { at: String, count: usize },
    #[error("{at}: a node has either one value or children, not both")]
    ValueAndChildren { at: String },
    #[error("{at}: type-annotated values aren't supported")]
    AnnotatedValue { at: String },
    #[error("{at}: {error}")]
    IntRange { at: String, error: IntError },
    #[error("line {line}, column {column}: nested deeper than {MAX_DEPTH} levels")]
    TooDeep { line: usize, column: usize },
}

impl Diagnostic for ParseError {
    fn code(&self) -> &'static str {
        match self {
            Self::Syntax { .. } => "kdl.parse.syntax",
            Self::DuplicateKey { .. } => "kdl.parse.duplicate-key",
            Self::Properties { .. } => "kdl.parse.properties",
            Self::UnknownAnnotation { .. } => "kdl.parse.unknown-annotation",
            Self::ListArguments { .. } => "kdl.parse.list-arguments",
            Self::ListItemName { .. } => "kdl.parse.list-item-name",
            Self::ArgumentCount { .. } => "kdl.parse.argument-count",
            Self::ValueAndChildren { .. } => "kdl.parse.value-and-children",
            Self::AnnotatedValue { .. } => "kdl.parse.annotated-value",
            Self::IntRange { .. } => "kdl.parse.int-range",
            Self::TooDeep { .. } => "kdl.parse.too-deep",
        }
    }

    fn message(&self) -> String {
        self.to_string()
    }

    fn hint(&self) -> Option<String> {
        Some(
            match self {
                Self::Syntax { .. } => "fix the syntax at the reported place",
                Self::DuplicateKey { .. } => "remove or rename one of the entries",
                Self::Properties { .. } => "write each setting as its own node",
                Self::UnknownAnnotation { .. } => "remove the annotation; only (list) is used",
                Self::ListArguments { .. } => "put the items in a block of '-' nodes",
                Self::ListItemName { .. } => "name every list item '-'",
                Self::ArgumentCount { .. } => "give the node one value, or a block of children",
                Self::ValueAndChildren { .. } => "keep either the value or the children",
                Self::AnnotatedValue { .. } => "remove the annotation from the value",
                Self::IntRange { .. } => "use a larger integer",
                Self::TooDeep { .. } => "flatten the nested blocks, comments or '/-' marks",
            }
            .to_owned(),
        )
    }
}

/// The `Document` that `text` spells.
///
/// # Errors
/// `TooDeep` for blocks, block comments or `/-` marks nested beyond [`MAX_DEPTH`], `Syntax` for
/// text that is not KDL v2, and a shape error naming the offending node (as `document > a > b`,
/// list items as `a[0]`) for KDL outside the canonical shapes.
pub fn parse(text: &str) -> Result<Document, ParseError> {
    nesting::check(text).map_err(|offset| {
        let (line, column) = line_column(text, offset);
        ParseError::TooDeep { line, column }
    })?;
    let parsed = KdlDocument::parse_v2(text).map_err(|error| ParseError::Syntax {
        detail: syntax_detail(&error, text),
    })?;
    dict(parsed.nodes(), "document")
}

/// The crate's diagnostics, each with the line and column it points at.
fn syntax_detail(error: &kdl::KdlError, text: &str) -> String {
    let details: Vec<String> = error
        .diagnostics
        .iter()
        .map(|d| {
            let (line, column) = line_column(text, d.span.offset());
            format!("{d} (line {line}, column {column})")
        })
        .collect();
    details.join("; ")
}

/// The 1-based line and column (in characters) of byte `offset`; past the end means the end.
fn line_column(text: &str, offset: usize) -> (usize, usize) {
    let before = String::from_utf8_lossy(text.as_bytes().get(..offset).unwrap_or(text.as_bytes()));
    let line = before.matches('\n').count().saturating_add(1);
    let column = before
        .chars()
        .rev()
        .take_while(|c| *c != '\n')
        .count()
        .saturating_add(1);
    (line, column)
}

fn dict(nodes: &[KdlNode], at: &str) -> Result<Document, ParseError> {
    let mut entries = Vec::with_capacity(nodes.len());
    for child in nodes {
        let name = child.name().value();
        let value = node(child, &format!("{at} > {name}"))?;
        entries.push((name.to_owned(), value));
    }
    Document::from_entries(entries).map_err(|DocumentError::DuplicateKey { key }| {
        ParseError::DuplicateKey {
            at: at.to_owned(),
            key,
        }
    })
}

fn node(node: &KdlNode, at: &str) -> Result<Value, ParseError> {
    let properties: Vec<String> = node
        .entries()
        .iter()
        .filter_map(|entry| entry.name().map(|name| name.value().to_owned()))
        .collect();
    if !properties.is_empty() {
        return Err(ParseError::Properties {
            at: at.to_owned(),
            names: properties,
        });
    }
    let children = node.children().map_or(&[][..], KdlDocument::nodes);
    match node.ty().map(KdlIdentifier::value) {
        None => {}
        Some(LIST_TAG) => {
            if !node.entries().is_empty() {
                return Err(ParseError::ListArguments { at: at.to_owned() });
            }
            return list(children, at).map(Value::List);
        }
        Some(other) => {
            return Err(ParseError::UnknownAnnotation {
                at: at.to_owned(),
                annotation: other.to_owned(),
            });
        }
    }
    match node.entries() {
        [] => dict(children, at).map(Value::Dict),
        [arg] if node.children().is_none() => scalar(arg, at),
        [_] => Err(ParseError::ValueAndChildren { at: at.to_owned() }),
        args => Err(ParseError::ArgumentCount {
            at: at.to_owned(),
            count: args.len(),
        }),
    }
}

fn list(items: &[KdlNode], at: &str) -> Result<Vec<Value>, ParseError> {
    let mut out = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let here = format!("{at}[{index}]");
        if item.name().value() != ITEM {
            return Err(ParseError::ListItemName {
                at: here,
                name: item.name().value().to_owned(),
            });
        }
        out.push(node(item, &here)?);
    }
    Ok(out)
}

fn scalar(entry: &KdlEntry, at: &str) -> Result<Value, ParseError> {
    if entry.ty().is_some() {
        return Err(ParseError::AnnotatedValue { at: at.to_owned() });
    }
    value_of(entry.value(), at)
}

fn value_of(value: &KdlValue, at: &str) -> Result<Value, ParseError> {
    Ok(match value {
        KdlValue::String(text) => Value::Str(text.clone()),
        KdlValue::Integer(n) => {
            Value::Int(Int::parse(*n).map_err(|error| ParseError::IntRange {
                at: at.to_owned(),
                error,
            })?)
        }
        KdlValue::Float(f) => Value::Float(Float::new(*f)),
        KdlValue::Bool(b) => Value::Bool(*b),
        KdlValue::Null => Value::Null,
    })
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod tests {
    use proptest::prelude::*;

    use super::super::emit::emit;
    use super::super::fixtures::{doc, document_from_golden, int, value_from_golden};
    use super::*;
    use crate::testing::{self, Entry};

    fn witness(e: &ParseError) -> &'static str {
        match e {
            ParseError::Syntax { .. } => "Syntax",
            ParseError::DuplicateKey { .. } => "DuplicateKey",
            ParseError::Properties { .. } => "Properties",
            ParseError::UnknownAnnotation { .. } => "UnknownAnnotation",
            ParseError::ListArguments { .. } => "ListArguments",
            ParseError::ListItemName { .. } => "ListItemName",
            ParseError::ArgumentCount { .. } => "ArgumentCount",
            ParseError::ValueAndChildren { .. } => "ValueAndChildren",
            ParseError::AnnotatedValue { .. } => "AnnotatedValue",
            ParseError::IntRange { .. } => "IntRange",
            ParseError::TooDeep { .. } => "TooDeep",
        }
    }

    fn at(path: &str) -> String {
        path.to_owned()
    }

    fn errors() -> Vec<ParseError> {
        vec![
            ParseError::Syntax {
                detail: "Expected identifier string (line 1, column 3)".to_owned(),
            },
            ParseError::DuplicateKey {
                at: at("document"),
                key: "a".to_owned(),
            },
            ParseError::Properties {
                at: at("document > a"),
                names: vec!["x".to_owned(), "y".to_owned()],
            },
            ParseError::UnknownAnnotation {
                at: at("document > a"),
                annotation: "u8".to_owned(),
            },
            ParseError::ListArguments {
                at: at("document > a"),
            },
            ParseError::ListItemName {
                at: at("document > a[0]"),
                name: "x".to_owned(),
            },
            ParseError::ArgumentCount {
                at: at("document > a"),
                count: 2,
            },
            ParseError::ValueAndChildren {
                at: at("document > a"),
            },
            ParseError::AnnotatedValue {
                at: at("document > a"),
            },
            ParseError::IntRange {
                at: at("document > a"),
                error: IntError::Smallest,
            },
            ParseError::TooDeep { line: 3, column: 7 },
        ]
    }

    /// One catalogue entry per variant.
    pub(crate) fn entries() -> Vec<Entry> {
        errors().iter().map(Entry::of).collect()
    }

    #[test]
    fn every_error_variant_has_a_table_case() {
        testing::every_variant(
            &errors(),
            witness,
            &[
                "Syntax",
                "DuplicateKey",
                "Properties",
                "UnknownAnnotation",
                "ListArguments",
                "ListItemName",
                "ArgumentCount",
                "ValueAndChildren",
                "AnnotatedValue",
                "IntRange",
                "TooDeep",
            ],
        );
    }

    #[test]
    fn messages_name_the_node_and_the_problem() {
        testing::table(
            &[
                (
                    ParseError::Properties {
                        at: at("document > a"),
                        names: vec!["x".to_owned(), "y".to_owned()],
                    },
                    "document > a: properties (x, y) aren't supported".to_owned(),
                ),
                (
                    ParseError::ListItemName {
                        at: at("document > l[2]"),
                        name: "x".to_owned(),
                    },
                    "document > l[2]: list items must be named '-', got \"x\"".to_owned(),
                ),
                (
                    ParseError::ArgumentCount {
                        at: at("document > a"),
                        count: 3,
                    },
                    "document > a: expected one value, got 3".to_owned(),
                ),
                (
                    ParseError::TooDeep { line: 3, column: 7 },
                    "line 3, column 7: nested deeper than 16 levels".to_owned(),
                ),
            ],
            Diagnostic::message,
        );
    }

    fn parsed(text: &str) -> Result<Document, ParseError> {
        parse(text)
    }

    #[test]
    fn the_example_document_parses_back() {
        let expected = doc(&[
            (
                "ssh",
                Value::Dict(doc(&[("port", Value::Int(int(22))), ("host", Value::Null)])),
            ),
            (
                "xs",
                Value::List(vec![Value::Bool(true), Value::Float(Float::new(1.5))]),
            ),
        ]);
        let text = "ssh {\n    port 22\n    host #null\n}\n(list)xs {\n    - #true\n    - 1.5\n}\n";
        assert_eq!(parsed(text), Ok(expected));
    }

    #[test]
    fn non_canonical_layout_and_comments_are_accepted() {
        let text =
            "ssh { port 22; } // trailing comment\n/- dropped \"node\"\n(list)xs { - 1; - 2 }";
        assert_eq!(
            parsed(text),
            Ok(doc(&[
                ("ssh", Value::Dict(doc(&[("port", Value::Int(int(22)))]))),
                (
                    "xs",
                    Value::List(vec![Value::Int(int(1)), Value::Int(int(2))]),
                ),
            ]))
        );
    }

    #[test]
    fn an_empty_text_is_the_empty_document() {
        assert_eq!(parsed(""), Ok(Document::new()));
        assert_eq!(parsed("// nothing\n"), Ok(Document::new()));
    }

    #[test]
    fn empty_blocks_are_empty_dicts_and_empty_lists() {
        assert_eq!(
            parsed("a {}\n(list)b {}\n(list)c\na\n"),
            Err(ParseError::DuplicateKey {
                at: at("document"),
                key: "a".to_owned(),
            })
        );
        assert_eq!(
            parsed("a {}\n(list)b {}\n(list)c\n"),
            Ok(doc(&[
                ("a", Value::Dict(Document::new())),
                ("b", Value::List(vec![])),
                ("c", Value::List(vec![])),
            ]))
        );
    }

    #[test]
    fn accepted_corpus_parses_to_the_listed_values() {
        let cases: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../../tests/golden/kdl_accepted.json")).unwrap();
        assert!(cases.len() >= 10, "corpus went missing: {}", cases.len());
        for case in cases {
            let field = |key: &str| case.get(key).and_then(serde_json::Value::as_str).unwrap();
            let (name, text) = (field("name"), field("text"));
            let expected = value_from_golden(case.get("value").unwrap());
            assert_eq!(
                parsed(text).map(Value::Dict),
                Ok(expected),
                "case {name}: {text:?}"
            );
        }
    }

    #[test]
    fn rejected_corpus_is_refused() {
        let cases: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../../tests/golden/kdl_rejected.json")).unwrap();
        assert!(cases.len() >= 15, "corpus went missing: {}", cases.len());
        for case in cases {
            let field = |key: &str| case.get(key).and_then(serde_json::Value::as_str).unwrap();
            let (name, text) = (field("name"), field("text"));
            let result = parsed(text);
            assert!(
                result.is_err(),
                "case {name} accepted {text:?} as {result:?}"
            );
        }
    }

    #[test]
    fn canonical_corpus_parses_to_its_documents_and_emits_byte_for_byte() {
        let cases = testing::golden_cases("kdl", "json", "kdl").unwrap();
        assert!(cases.len() >= 20, "corpus went missing: {}", cases.len());
        for case in cases {
            let json: serde_json::Value = serde_json::from_str(&case.input).unwrap();
            let expected = document_from_golden(&json);
            let back = parsed(&case.output);
            assert_eq!(back.as_ref(), Ok(&expected), "case {}", case.name);
            assert_eq!(emit(&expected), case.output, "case {}", case.name);
        }
    }

    /// `text` is refused with exactly `expected`.
    #[track_caller]
    fn refuses(text: &str, expected: ParseError) {
        assert_eq!(parsed(text), Err(expected), "input {text:?}");
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|n| (*n).to_owned()).collect()
    }

    fn argument_count(path: &str, count: usize) -> ParseError {
        ParseError::ArgumentCount {
            at: at(path),
            count,
        }
    }

    fn duplicate(path: &str) -> ParseError {
        ParseError::DuplicateKey {
            at: at(path),
            key: "a".to_owned(),
        }
    }

    fn annotation(path: &str, name: &str) -> ParseError {
        ParseError::UnknownAnnotation {
            at: at(path),
            annotation: name.to_owned(),
        }
    }

    fn properties(path: &str, list: &[&str]) -> ParseError {
        ParseError::Properties {
            at: at(path),
            names: names(list),
        }
    }

    fn list_item_name(path: &str, name: &str) -> ParseError {
        ParseError::ListItemName {
            at: at(path),
            name: name.to_owned(),
        }
    }

    #[test]
    fn two_arguments_are_refused() {
        refuses("a 1 2", argument_count("document > a", 2));
        refuses("a 1 2 3", argument_count("document > a", 3));
    }

    #[test]
    fn a_property_is_refused() {
        refuses("a x=1", properties("document > a", &["x"]));
    }

    #[test]
    fn every_property_is_named_in_order_even_beside_an_argument() {
        refuses("a 1 y=2 x=3", properties("document > a", &["y", "x"]));
    }

    #[test]
    fn an_argument_next_to_children_is_refused() {
        let error = ParseError::ValueAndChildren {
            at: at("document > a"),
        };
        refuses("a 1 { b 2 }", error.clone());
        refuses("a 1 {}", error);
    }

    #[test]
    fn a_repeated_top_level_key_is_refused() {
        refuses("a 1\na 2", duplicate("document"));
    }

    #[test]
    fn a_repeated_nested_key_is_refused_naming_its_parent() {
        refuses("o {\n    a 1\n    a 2\n}\n", duplicate("document > o"));
    }

    #[test]
    fn keys_repeat_after_unquoting() {
        refuses("a 1\n\"a\" 2", duplicate("document"));
    }

    #[test]
    fn an_unknown_type_annotation_on_a_node_is_refused() {
        refuses("(u8)a 1", annotation("document > a", "u8"));
    }

    #[test]
    fn a_type_annotated_value_is_refused() {
        let error = ParseError::AnnotatedValue {
            at: at("document > a"),
        };
        refuses("a (u8)1", error.clone());
        refuses("a (list)1", error);
    }

    #[test]
    fn a_list_item_not_named_dash_is_refused_with_its_index() {
        refuses("(list)a { x 1 }", list_item_name("document > a[0]", "x"));
        refuses(
            "(list)a { - 1; \"-x\" 2 }",
            list_item_name("document > a[1]", "-x"),
        );
    }

    #[test]
    fn a_list_node_with_an_argument_is_refused() {
        let error = ParseError::ListArguments {
            at: at("document > a"),
        };
        refuses("(list)a 1", error.clone());
        refuses("(list)a 1 { - 2 }", error);
    }

    #[test]
    fn a_list_node_with_a_property_is_refused_as_a_property() {
        refuses("(list)a x=1", properties("document > a", &["x"]));
    }

    #[test]
    fn a_list_item_with_a_property_is_refused() {
        refuses(
            "(list)l {\n    - k=1\n}\n",
            properties("document > l[0]", &["k"]),
        );
    }

    #[test]
    fn a_list_item_with_an_unknown_annotation_is_refused() {
        refuses("(list)l { (u8)- 1 }", annotation("document > l[0]", "u8"));
    }

    #[test]
    fn a_list_item_with_two_arguments_is_refused() {
        refuses("(list)l { - 1 2 }", argument_count("document > l[0]", 2));
    }

    #[test]
    fn errors_inside_a_list_item_name_the_path_through_it() {
        refuses(
            "(list)l { - { k 1 1 } }",
            argument_count("document > l[0] > k", 2),
        );
        refuses(
            "a { (list)b { - { c (u8)1 } } }",
            ParseError::AnnotatedValue {
                at: at("document > a > b[0] > c"),
            },
        );
    }

    #[test]
    fn a_repeated_key_inside_a_list_item_is_refused() {
        refuses("(list)l { - { a 1; a 2 } }", duplicate("document > l[0]"));
    }

    #[test]
    fn the_first_problem_in_document_order_is_the_one_reported() {
        refuses("a 1 2\nb x=1\n", argument_count("document > a", 2));
        refuses(
            "o { b x=1 }\na 1 2\n",
            properties("document > o > b", &["x"]),
        );
    }

    #[test]
    fn syntax_errors_say_what_and_where() {
        for (text, detail) in [
            (
                "a {\n",
                "No closing '}' for child block (line 1, column 3); \
                 Closing '}' was not found after nodes (line 1, column 4)",
            ),
            ("a true\n", "Expected identifier string (line 1, column 3)"),
            (
                "ok 1\na \"x\n",
                "Unexpected newline in single-line quoted string (line 2, column 3)",
            ),
        ] {
            assert_eq!(
                parsed(text),
                Err(ParseError::Syntax {
                    detail: detail.to_owned()
                }),
                "{text:?}"
            );
        }
    }

    #[test]
    fn line_and_column_count_characters_and_clamp_to_the_end() {
        testing::table(
            &[
                (("", 0), (1, 1)),
                (("abc", 0), (1, 1)),
                (("abc", 2), (1, 3)),
                (("abc", 3), (1, 4)),
                (("abc", 99), (1, 4)),
                (("a\nbc", 2), (2, 1)),
                (("a\nbc", 3), (2, 2)),
                (("a\n\nb", 3), (3, 1)),
                (("é€x", 5), (1, 3)),
                (("é€x", 6), (1, 4)),
            ],
            |(text, offset)| line_column(text, *offset),
        );
    }

    #[test]
    fn other_syntax_the_crate_refuses_is_a_syntax_error() {
        for text in [
            "a 340282366920938463463374607431768211456\n",
            "a 170141183460469231731687303715884105728\n",
            "a -170141183460469231731687303715884105729\n",
            "a \"\\u{110000}\"\n",
            "/* never closed\n",
        ] {
            assert!(
                matches!(parsed(text), Err(ParseError::Syntax { .. })),
                "{text:?}"
            );
        }
    }

    /// `n` nested nodes: `a { a { ... a {} } }`.
    fn chain(n: usize) -> String {
        format!("{}{}", "a {".repeat(n), "}".repeat(n))
    }

    /// `n` nested lists: `(list)a { (list)- { ... (list)- {} } }`.
    fn list_chain(n: usize) -> String {
        format!(
            "(list)a {{{}{}",
            " (list)- {".repeat(n.saturating_sub(1)),
            "}".repeat(n)
        )
    }

    #[test]
    fn nesting_is_bounded_at_max_depth_for_dicts_and_lists() {
        assert!(parsed(&chain(MAX_DEPTH)).is_ok());
        assert_eq!(
            parsed(&chain(MAX_DEPTH + 1)),
            Err(ParseError::TooDeep {
                line: 1,
                column: 51
            })
        );
        assert!(parsed(&list_chain(MAX_DEPTH)).is_ok());
        assert_eq!(
            parsed(&list_chain(MAX_DEPTH + 1)),
            Err(ParseError::TooDeep {
                line: 1,
                column: 169
            })
        );
    }

    #[test]
    fn the_crates_recursive_constructs_are_bounded_before_it_runs() {
        let comments = format!("{}{}\na 1\n", "/* ".repeat(500), "*/ ".repeat(500));
        let dashes = format!("{}a 1\n", "/- ".repeat(500));
        for (text, column) in [(comments, 49), (dashes, 49), (chain(500), 51)] {
            assert_eq!(parsed(&text), Err(ParseError::TooDeep { line: 1, column }));
        }
    }

    #[test]
    fn scalars_keep_their_kind_and_exact_value() {
        let text = "\
            n #null\nt #true\nf #false\nzero 0\nneg -5\nhex 0x10\n\
            max 170141183460469231731687303715884105727\n\
            min -170141183460469231731687303715884105727\n\
            pi 3.5\nnz -0.0\nbig 1e+300\ntiny 5e-324\n\"nan\" #nan\n\"inf\" #inf\nninf #-inf\n\
            s \"x\"\nbare alpha\nraw #\"a\\b\"#\nesc \"\\u{1F680}\\n\"\n";
        let expected = doc(&[
            ("n", Value::Null),
            ("t", Value::Bool(true)),
            ("f", Value::Bool(false)),
            ("zero", Value::Int(int(0))),
            ("neg", Value::Int(int(-5))),
            ("hex", Value::Int(int(16))),
            ("max", Value::Int(int(i128::MAX))),
            (
                "min",
                Value::Int(int(-170_141_183_460_469_231_731_687_303_715_884_105_727)),
            ),
            ("pi", Value::Float(Float::new(3.5))),
            ("nz", Value::Float(Float::new(-0.0))),
            ("big", Value::Float(Float::new(1e300))),
            ("tiny", Value::Float(Float::new(5e-324))),
            ("nan", Value::Float(Float::new(f64::NAN))),
            ("inf", Value::Float(Float::new(f64::INFINITY))),
            ("ninf", Value::Float(Float::new(f64::NEG_INFINITY))),
            ("s", Value::Str("x".to_owned())),
            ("bare", Value::Str("alpha".to_owned())),
            ("raw", Value::Str("a\\b".to_owned())),
            ("esc", Value::Str("🚀\n".to_owned())),
        ]);
        assert_eq!(parsed(text), Ok(expected));
    }

    #[test]
    fn an_integer_outside_the_refinement_is_refused_naming_the_node() {
        assert_eq!(
            value_of(&KdlValue::Integer(i128::MIN), "document > a"),
            Err(ParseError::IntRange {
                at: at("document > a"),
                error: IntError::Smallest,
            })
        );
        assert_eq!(
            value_of(&KdlValue::Integer(-7), "document > a"),
            Ok(Value::Int(int(-7)))
        );
    }

    #[test]
    fn every_emitted_float_spelling_is_read_back_bit_exactly() {
        let samples = [
            0.0,
            -0.0,
            0.1,
            0.2 + 0.1,
            1.0 / 3.0,
            2.0_f64.powi(53),
            2.0_f64.powi(-1074),
            1e22,
            1e23,
            9_007_199_254_740_993.0,
            f64::MAX,
            f64::MIN,
            f64::MIN_POSITIVE,
            f64::EPSILON,
            -123.456e-7,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ];
        for value in samples {
            let original = doc(&[("f", Value::Float(Float::new(value)))]);
            assert_eq!(parsed(&emit(&original)), Ok(original), "{value:?}");
        }
    }

    fn scalar_value() -> impl Strategy<Value = Value> {
        prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::Bool),
            prop_oneof![
                any::<i128>(),
                Just(i128::MAX),
                Just(-170_141_183_460_469_231_731_687_303_715_884_105_727),
                -1000_i128..1000,
            ]
            .prop_filter_map("the smallest integer", |n| Int::parse(n)
                .ok()
                .map(Value::Int)),
            any::<f64>().prop_map(|f| Value::Float(Float::new(f))),
            any::<String>().prop_map(Value::Str),
        ]
    }

    fn key() -> impl Strategy<Value = String> {
        prop_oneof![
            any::<String>(),
            "[A-Za-z_][A-Za-z0-9_-]{0,6}",
            Just("-".to_owned()),
            Just("true".to_owned()),
            Just(String::new()),
        ]
    }

    fn dedup(pairs: Vec<(String, Value)>) -> Document {
        let mut out = Document::new();
        for (name, value) in pairs {
            if out.get(&name).is_none() {
                assert!(out.insert(name, value).is_ok());
            }
        }
        out
    }

    fn value() -> impl Strategy<Value = Value> {
        scalar_value().prop_recursive(4, 32, 4, |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..4).prop_map(Value::List),
                prop::collection::vec((key(), inner), 0..4)
                    .prop_map(|pairs| Value::Dict(dedup(pairs))),
            ]
        })
    }

    fn document() -> impl Strategy<Value = Document> {
        prop::collection::vec((key(), value()), 0..5).prop_map(dedup)
    }

    proptest! {
        #[test]
        fn parse_of_emit_is_the_identity(d in document()) {
            let text = emit(&d);
            prop_assert_eq!(parse(&text), Ok(d));
        }

        #[test]
        fn emit_of_parse_is_the_identity_on_canonical_text(d in document()) {
            let text = emit(&d);
            let back = parse(&text);
            prop_assert_eq!(back.as_ref().map(emit), Ok(text));
        }
    }
}
