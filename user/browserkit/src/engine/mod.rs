//! The page renderer, as the Browser sees it: `user/webengine` (its `src/api.rs` is the
//! interface). `Document::parse(bytes, url, content_type)` gives a page, the app fetches its
//! `pending_resources()` (stylesheets, images) and supplies them, `layout(viewport, measurer)`
//! gives a [`DisplayList`] the app paints (crate::paint), and `hit_test`, `find`, the form
//! controls and `submit` drive the rest. This module is the one place the app names the
//! engine, so a change of engine is a change here.

pub use webengine::{
    ControlId, ControlKind, DisplayList, Document, Entry, Family, FindMatch, Font, FontMetrics,
    FormControl, Hit, ImageId, Item, Link, Method, Rect, Resource, ResourceKind, Submission,
    TextMeasurer, Viewport, Storage,
};

/// A pixel colour: `0xAARRGGBB` in the engine's lists, `0x00RRGGBB` on the screen.
pub type Color = u32;
