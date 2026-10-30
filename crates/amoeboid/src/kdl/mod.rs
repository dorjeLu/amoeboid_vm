//! KDL v2 data model, canonical text form and parser adapter (design §Workspace `kdl/`).

mod emit;
mod nesting;
mod nonprintable;
mod parse;
mod value;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
pub(crate) mod fixtures;

pub use emit::emit;
pub use nesting::MAX_DEPTH;
pub use parse::{ParseError, parse};
pub use value::{Document, DocumentError, Float, Int, IntError, Value};

#[cfg(test)]
pub(crate) use parse::tests as parse_tests;
#[cfg(test)]
pub(crate) use value::tests as value_tests;
