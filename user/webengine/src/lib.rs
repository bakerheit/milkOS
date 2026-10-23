//! milkOS's web engine (see `api.rs` for the interface the Browser app uses).
//!
//! Pipeline: bytes → [`charset`] → [`html`] tokenizer + tree builder → [`dom`] → [`css`]
//! stylesheets → [`style`] (cascade, computed values) → [`layout`] (box tree, block/inline,
//! floats, positioning, tables, flexbox) → [`paint`] (the display list). [`page`] ties it
//! together with forms, hit testing and find in page.
//!
//! no_std + alloc and no dependencies, so it builds for the guest and tests on the Mac.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod api;
pub mod atoms;
pub mod charset;
pub mod css;
pub mod dom;
pub mod fmath;
mod forms;
pub mod html;
pub mod layout;
mod measure;
mod page;
mod paint;
pub mod style;
pub mod url;

#[cfg(test)]
mod tests;

pub use api::*;
