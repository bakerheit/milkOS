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

// Test-only: the kernel's baked system fonts and PNG encoder, for the screenshots the
// rendering tests write (tests/raster.rs).
#[cfg(test)]
mod sysfont {
    pub struct Face {
        pub size: u8,
        pub line: u8,
        pub ascent: u8,
        pub weight: u16,
        pub first: u16,
        pub count: u16,
    }
    pub struct Family {
        pub key: &'static str,
        pub name: &'static str,
        pub copyright: &'static str,
        pub licence: &'static str,
        pub blob: &'static [u8],
        pub faces: &'static [Face],
    }
}
#[cfg(test)]
#[allow(dead_code)]
#[path = "../../../kernel/src/wm/png.rs"]
mod kernel_png;
#[cfg(test)]
#[allow(dead_code)]
#[path = "../../../kernel/src/sysfont_data.rs"]
mod sysfont_data;

pub use api::*;
