//! Diagnostics and warnings to stderr.

// The single place that prints (design §CLI); the workspace denies printing everywhere else.
#![allow(clippy::print_stdout, clippy::print_stderr)]
