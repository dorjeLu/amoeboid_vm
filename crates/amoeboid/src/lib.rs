//! Management logic for amoeboid microVMs: presets, per-VM configs, builds, runs.

// Test code is excluded from coverage so pure files are measured on their own branches.
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod testing;

pub mod adapters;
pub mod config;
pub mod console;
pub mod contract;
pub mod diagnostic;
pub mod domain;
pub mod kdl;
pub mod layout;
pub mod ops;
pub mod plan;
pub mod ports;
pub mod rails;
pub mod records;
pub mod refined;
