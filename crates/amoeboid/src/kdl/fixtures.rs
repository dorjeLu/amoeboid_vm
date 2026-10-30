//! Test fixtures shared by the KDL tests: the golden corpus's JSON encoding of `Value`.
//!
//! `null`/bool/string as is, list as array, `{"int":"123"}`, `{"float":"repr"}` (`nan`, `inf`,
//! `-inf` spelled so), `{"dict":[[key,value],...]}` with the order kept.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "fixtures build test values; clippy does not treat this out-of-line module as test code"
)]

use serde_json::Value as Json;

use super::value::{Document, Float, Int, Value};

/// An integer that `Int` accepts.
pub(crate) fn int(n: i128) -> Int {
    Int::parse(n).unwrap()
}

/// A document from literal entries; keys must be distinct.
pub(crate) fn doc(entries: &[(&str, Value)]) -> Document {
    Document::from_entries(entries.iter().map(|(k, v)| ((*k).to_owned(), v.clone()))).unwrap()
}

pub(crate) fn value_from_golden(json: &Json) -> Value {
    match json {
        Json::Null => Value::Null,
        Json::Bool(b) => Value::Bool(*b),
        Json::String(s) => Value::Str(s.clone()),
        Json::Array(items) => Value::List(items.iter().map(value_from_golden).collect()),
        Json::Number(n) => panic!("bare number {n} in golden value"),
        Json::Object(map) => {
            if let Some(Json::String(n)) = map.get("int") {
                Value::Int(int(n.parse().unwrap()))
            } else if let Some(Json::String(f)) = map.get("float") {
                Value::Float(Float::new(match f.as_str() {
                    "nan" => f64::NAN,
                    "inf" => f64::INFINITY,
                    "-inf" => f64::NEG_INFINITY,
                    other => other.parse().unwrap(),
                }))
            } else {
                Value::Dict(document_from_golden(json))
            }
        }
    }
}

/// `json` must be a `{"dict": [...]}`.
pub(crate) fn document_from_golden(json: &Json) -> Document {
    let entries = json.get("dict").and_then(Json::as_array).unwrap();
    Document::from_entries(entries.iter().map(|pair| {
        let pair = pair.as_array().unwrap();
        let key = pair.first().and_then(Json::as_str).unwrap();
        (key.to_owned(), value_from_golden(pair.get(1).unwrap()))
    }))
    .unwrap()
}
