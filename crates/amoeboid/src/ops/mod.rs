//! Use cases (design §Workspace `ops/`): sequences of port calls around the pure decisions.
//! Decide functions live in `decide.rs` files so the coverage gate sees them.

pub mod build;
pub mod config;
pub mod home_files;
pub mod host_key;
pub mod nix_config;
pub mod preflight;
pub mod records;
