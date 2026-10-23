//! The Browser's rules and pixels without any I/O (see Cargo.toml): the app (`user/browser`)
//! does the networking, files and the window, and asks this crate what to do.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod address;
pub mod bookmarks;
pub mod certinfo;
pub mod cookies;
pub mod date;
pub mod download;
pub mod engine;
pub mod font;
pub mod form;
pub mod history;
pub mod netpolicy;
pub mod pages;
pub mod paint;
pub mod storage;

#[cfg(test)]
mod systext_tests;

use alloc::string::String;

/// `s` on one line: tabs, newlines and other control characters become spaces.
pub fn clean_line(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}
