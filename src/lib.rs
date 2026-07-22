//! Prist — a Flutter version manager in Rust.
//!
//! See `prist-spec-construccion.md` for the full design. This crate exposes
//! the library logic used by the `prist` CLI binary in `src/main.rs`.

#![allow(clippy::needless_doctest_main)]
#![allow(clippy::result_large_err)]

pub mod cli;
pub mod commands;
pub mod config;
pub mod engine;
pub mod error;
pub mod fs_util;
pub mod git_ops;
pub mod ide;
pub mod paths;
pub mod releases;

pub use error::{PristError, Result};
