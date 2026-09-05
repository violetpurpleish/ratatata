//! Parinfer Smart/Indent/Paren engine, vendored from parinfer-rust.
//!
//! The upstream crate is a Vim/Emacs plugin with C FFI bindings and is not
//! published on crates.io, so only the pure-Rust algorithm is kept here.

#![allow(dead_code)]
#![allow(clippy::all)]

mod changes;
pub mod parinfer;
pub mod types;
