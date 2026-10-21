//! Built-in kernel-side apps. Each is a [`WindowContents`]; `create` gives an app's starting
//! window rect and a fresh instance.

pub mod about;
pub mod files;
pub mod fsx;
pub mod notes;
pub mod terminal;

use alloc::boxed::Box;

use crate::gfx::Rect;

use super::window::{App, WindowContents};

pub fn create(app: App) -> (Rect, Box<dyn WindowContents>) {
    match app {
        App::Files => (Rect::new(96, 64, 600, 400), Box::new(files::Files::new())),
        App::Terminal => (
            Rect::new(440, 270, 520, 290),
            Box::new(terminal::Terminal::new()),
        ),
        App::Notes => (Rect::new(120, 96, 440, 330), Box::new(notes::Notes::new())),
        App::About => (Rect::new(332, 140, 360, 380), Box::new(about::About::new())),
    }
}
