//! The canonical KDL v2 emitter. Each dict entry becomes a node named after its key:
//!
//! ```text
//! scalar  ->  key "text" / key 42 / key 1.5 / key #true / key #null
//! dict    ->  key { child-entries... }        (empty: key {})
//! list    ->  (list)key { - item; - item }    (empty: (list)key {})
//! ```
//!
//! List items are nodes named `-` under the same rules, so lists and dicts nest freely; the
//! `(list)` annotation tells a list from a dict, so empty ones and dicts with a `-` key round-trip.
//! Emission is total: Rust strings hold no surrogates and every double has a shortest spelling.
//! Recursion depth equals nesting depth.

use super::nonprintable::is_nonprintable;
use super::value::{Document, Value};

pub(super) const LIST_TAG: &str = "list";
pub(super) const ITEM: &str = "-";
const INDENT: &str = "    ";
const KEYWORDS: [&str; 5] = ["true", "false", "null", "inf", "nan"];

/// `doc` as canonical KDL v2 text.
#[must_use]
pub fn emit(doc: &Document) -> String {
    let mut out = String::new();
    for (key, value) in doc.iter() {
        entry(&mut out, key, value, 0);
    }
    out
}

fn entry(out: &mut String, name: &str, value: &Value, depth: usize) {
    let pad = INDENT.repeat(depth);
    let head = identifier(name);
    match value {
        Value::Dict(doc) => block(out, &pad, &head, doc.iter(), depth),
        Value::List(items) => {
            let head = format!("({LIST_TAG}){head}");
            block(
                out,
                &pad,
                &head,
                items.iter().map(|item| (ITEM, item)),
                depth,
            );
        }
        Value::Null => line(out, &pad, &head, "#null"),
        Value::Bool(true) => line(out, &pad, &head, "#true"),
        Value::Bool(false) => line(out, &pad, &head, "#false"),
        Value::Int(n) => line(out, &pad, &head, &n.get().to_string()),
        Value::Float(f) => line(out, &pad, &head, &float(f.get())),
        Value::Str(text) => line(out, &pad, &head, &quote(text)),
    }
}

fn line(out: &mut String, pad: &str, head: &str, scalar: &str) {
    out.extend([pad, head, " ", scalar, "\n"]);
}

fn block<'a>(
    out: &mut String,
    pad: &str,
    head: &str,
    entries: impl Iterator<Item = (&'a str, &'a Value)>,
    depth: usize,
) {
    let mut entries = entries.peekable();
    if entries.peek().is_none() {
        out.extend([pad, head, " {}\n"]);
        return;
    }
    out.extend([pad, head, " {\n"]);
    for (key, value) in entries {
        entry(out, key, value, depth.saturating_add(1));
    }
    out.extend([pad, "}\n"]);
}

/// `name` bare when that is unambiguous KDL, quoted otherwise.
fn identifier(name: &str) -> String {
    if name == ITEM || (is_bare(name) && !KEYWORDS.contains(&name)) {
        name.to_owned()
    } else {
        quote(name)
    }
}

/// `[A-Za-z_][A-Za-z0-9_-]*`
fn is_bare(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len().saturating_add(2));
    out.push('"');
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            // Every code point KDL forbids literally (controls, direction marks, BOM) plus other
            // invisible ones that are clearer escaped.
            _ if is_nonprintable(ch) => {
                let escape = format!("\\u{{{:x}}}", u32::from(ch));
                out.push_str(&escape);
            }
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn float(value: f64) -> String {
    if value.is_nan() {
        "#nan".to_owned()
    } else if value == f64::INFINITY {
        "#inf".to_owned()
    } else if value == f64::NEG_INFINITY {
        "#-inf".to_owned()
    } else {
        repr(value)
    }
}

/// The shortest decimal that reads back as exactly `value`, spelled like Python's `repr` (the
/// golden corpus comes from it): positional for decimal exponents -5 < e < 16, else `d.ddde+XX`.
/// Always contains `.` or `e`, so it stays a float in KDL. `value` must be finite.
fn repr(value: f64) -> String {
    // Rust's `{:e}` prints the shortest round-trip digits as `d.ddde-N`.
    let scientific = format!("{:e}", value.abs());
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let sign = if value.is_sign_negative() { "-" } else { "" };
    // value = 0.DIGITS * 10^point
    let point = exponent.saturating_add(1);
    let body = if point <= -4 || point > 16 {
        let (first, rest) = digits.split_at_checked(1).unwrap_or((&digits, ""));
        let fraction = if rest.is_empty() {
            String::new()
        } else {
            format!(".{rest}")
        };
        let sign = if exponent < 0 { '-' } else { '+' };
        format!("{first}{fraction}e{sign}{:02}", exponent.unsigned_abs())
    } else {
        let len = i32::try_from(digits.len()).unwrap_or(i32::MAX);
        if point <= 0 {
            format!(
                "0.{}{digits}",
                "0".repeat(usize::try_from(point.unsigned_abs()).unwrap_or(0))
            )
        } else if point >= len {
            let zeros = usize::try_from(point.saturating_sub(len)).unwrap_or(0);
            format!("{digits}{}.0", "0".repeat(zeros))
        } else {
            let at = usize::try_from(point).unwrap_or(0);
            let (whole, fraction) = digits.split_at_checked(at).unwrap_or((&digits, ""));
            format!("{whole}.{fraction}")
        }
    };
    format!("{sign}{body}")
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::super::fixtures::{doc, document_from_golden, int};
    use super::*;
    use crate::kdl::value::Float;
    use crate::testing;

    /// Like `testing::table` for functions returning owned text.
    fn text_table<I: std::fmt::Debug>(cases: &[(I, &str)], run: impl Fn(&I) -> String) {
        for (index, (input, expected)) in cases.iter().enumerate() {
            assert_eq!(&run(input), expected, "case {index}: input {input:?}");
        }
    }

    #[test]
    fn golden_kdl_corpus_is_emitted_byte_for_byte() {
        let cases = testing::golden_cases("kdl", "json", "kdl").unwrap();
        assert!(
            cases.len() >= 20,
            "corpus went missing: {} cases",
            cases.len()
        );
        for case in cases {
            let json: serde_json::Value = serde_json::from_str(&case.input).unwrap();
            let document = document_from_golden(&json);
            assert_eq!(emit(&document), case.output, "case {}", case.name);
        }
    }

    #[test]
    fn identifiers_are_bare_only_when_unambiguous() {
        text_table(
            &[
                ("name", "name"),
                ("_x", "_x"),
                ("a-b_c9", "a-b_c9"),
                ("-", "-"),
                ("--", "\"--\""),
                ("", "\"\""),
                ("9a", "\"9a\""),
                ("-a", "\"-a\""),
                ("a b", "\"a b\""),
                ("a.b", "\"a.b\""),
                ("é", "\"é\""),
                ("true", "\"true\""),
                ("false", "\"false\""),
                ("null", "\"null\""),
                ("inf", "\"inf\""),
                ("nan", "\"nan\""),
                ("True", "True"),
                ("nulls", "nulls"),
                ("a\"b", "\"a\\\"b\""),
            ],
            |name| identifier(name),
        );
    }

    #[test]
    fn strings_escape_what_kdl_requires_and_nothing_else() {
        text_table(
            &[
                ("", r#""""#),
                ("plain", r#""plain""#),
                ("a b", r#""a b""#),
                ("a\\b", r#""a\\b""#),
                ("a\"b", r#""a\"b""#),
                ("a\nb\rc\td", r#""a\nb\rc\td""#),
                ("\u{8}\u{c}", r#""\b\f""#),
                ("\u{0}\u{1b}\u{7f}", r#""\u{0}\u{1b}\u{7f}""#),
                ("a\u{a0}b", r#""a\u{a0}b""#),
                (
                    "\u{200b}\u{2028}\u{feff}\u{202e}",
                    r#""\u{200b}\u{2028}\u{feff}\u{202e}""#,
                ),
                ("é€😀", "\"é€😀\""),
                ("\u{10ffff}", r#""\u{10ffff}""#),
                ("${x} `y` #z /*", "\"${x} `y` #z /*\""),
            ],
            |text| quote(text),
        );
    }

    #[test]
    fn floats_are_spelled_like_python_repr() {
        text_table(
            &[
                (0.0, "0.0"),
                (-0.0, "-0.0"),
                (1.0, "1.0"),
                (-1.5, "-1.5"),
                (0.5, "0.5"),
                (0.1, "0.1"),
                (100.0, "100.0"),
                (0.0001, "0.0001"),
                (0.00012, "0.00012"),
                (0.00001, "1e-05"),
                (0.000_012_3, "1.23e-05"),
                (1e15, "1000000000000000.0"),
                (1.5e15, "1500000000000000.0"),
                (1e16, "1e+16"),
                (1.5e16, "1.5e+16"),
                (123_456_789.123_456_79, "123456789.12345679"),
                (1e100, "1e+100"),
                (1e300, "1e+300"),
                (5e-324, "5e-324"),
                (f64::MAX, "1.7976931348623157e+308"),
                (f64::MIN_POSITIVE, "2.2250738585072014e-308"),
                (f64::NAN, "#nan"),
                (f64::INFINITY, "#inf"),
                (f64::NEG_INFINITY, "#-inf"),
            ],
            |value| float(*value),
        );
    }

    #[test]
    fn every_finite_float_spelling_reads_back_exactly_and_stays_a_float() {
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
            f64::MIN_POSITIVE,
            f64::EPSILON,
            -123.456e-7,
        ];
        for value in samples {
            let text = float(value);
            let back: f64 = text.parse().unwrap();
            assert_eq!(back.to_bits(), value.to_bits(), "{value:?} spelled {text}");
            assert!(text.contains('.') || text.contains('e'), "{text}");
        }
    }

    #[test]
    fn scalars_dicts_and_lists_nest() {
        let nested = doc(&[
            ("a", Value::Int(int(-5))),
            ("b", Value::Float(Float::new(2.5))),
            (
                "c",
                Value::List(vec![
                    Value::Null,
                    Value::Bool(true),
                    Value::Bool(false),
                    Value::List(vec![]),
                    Value::Dict(doc(&[])),
                    Value::Dict(doc(&[("-", Value::Str("x".to_owned()))])),
                ]),
            ),
            (
                "d e",
                Value::Dict(doc(&[("f", Value::Int(int(i128::MAX)))])),
            ),
        ]);
        let expected = format!(
            "a -5\nb 2.5\n(list)c {{\n    - #null\n    - #true\n    - #false\n    (list)- {{}}\n    \
             - {{}}\n    - {{\n        - \"x\"\n    }}\n}}\n\"d e\" {{\n    f {}\n}}\n",
            i128::MAX
        );
        assert_eq!(emit(&nested), expected);
    }

    #[test]
    fn an_empty_document_emits_nothing() {
        assert_eq!(emit(&Document::new()), "");
    }
}
