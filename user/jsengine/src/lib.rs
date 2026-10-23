//! milkOS's JavaScript engine (see `api.rs` for the interface embedders use).
//!
//! Pipeline: source → [`lexer`] (tokens, with automatic semicolon insertion decided by the
//! parser) → [`parser`] (an AST, [`ast`]) → [`compiler`] (scope analysis and bytecode,
//! [`bytecode`]) → the VM in [`interp`], over the object model in [`object`] and the heap and
//! garbage collector in [`heap`]. The builtins live in [`builtins`].
//!
//! no_std + alloc and no dependencies, so it builds for the guest and tests on the Mac.

#![cfg_attr(not(test), no_std)]
#![allow(clippy::wrong_self_convention)]
// While the engine is being built behind the API stub.
#![allow(dead_code)]

extern crate alloc;

pub mod api;
mod ast;
mod atoms;
mod builtins;
mod bytecode;
mod compiler;
mod conv;
mod errors;
mod eval;
mod heap;
mod interp;
mod lexer;
mod libm;
mod math;
mod names;
mod numconv;
mod object;
mod parser;
mod realm;
mod regexp;
mod resolve;
mod stack;
mod string;
mod value;

#[cfg(test)]
mod tests;

pub use api::*;
