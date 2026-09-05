//! Parinfer Smart/Indent/Paren engine, vendored from parinfer-rust.
//!
//! The upstream crate is a Vim/Emacs plugin with C FFI bindings and is not
//! published on crates.io, so only the pure-Rust algorithm is kept here.
//!
//! Upstream: <https://github.com/eraserhd/parinfer-rust>
//! Commit: `0d334950099cd30a8e1ca520026f8da9ea1382fa`
//!
//! Local delta: [`changes::compute_text_changes`] reports display-column `x`
//! (grapheme width) instead of `+1` per Rust `char`, and counts `line_no`
//! from `'\n'` bytes so CRLF is not treated as a single grapheme. See that
//! module.

#![allow(dead_code)]
#![allow(clippy::all)]

mod changes;
pub mod parinfer;
pub mod types;
