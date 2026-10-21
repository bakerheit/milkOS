//! Built-in kernel-side apps. Each is a [`WindowContents`]; `create` gives an app's starting
//! window rect and a fresh instance.

pub mod about;
pub mod browser;
pub mod files;
pub mod fsx;
pub mod milkdrop;
pub mod music;
pub mod notes;
pub mod settings;
pub mod terminal;
pub mod versions;
pub mod welcome;

use alloc::boxed::Box;

use crate::gfx::Rect;

use super::window::{App, WindowContents};

pub fn create(app: App) -> (Rect, Box<dyn WindowContents>) {
    match app {
        App::Files => (Rect::new(96, 64, 600, 440), Box::new(files::Files::new())),
        App::Terminal => (
            Rect::new(440, 270, 520, 290),
            Box::new(terminal::Terminal::new()),
        ),
        App::Notes => (Rect::new(120, 96, 440, 330), Box::new(notes::Notes::new())),
        App::About => (Rect::new(332, 140, 360, 380), Box::new(about::About::new())),
        App::Browser => (
            Rect::new(150, 56, 700, 520),
            Box::new(browser::Browser::new()),
        ),
        App::Music => (Rect::new(300, 92, 460, 476), Box::new(music::Music::new())),
        App::Versions => (
            Rect::new(110, 64, 800, 500),
            Box::new(versions::Versions::new()),
        ),
        App::Welcome => (
            Rect::new((super::theme::screen_w() - 740) / 2, 40, 740, 606),
            Box::new(welcome::Welcome::new()),
        ),
        App::Powerbox => (
            Rect::new(192, 120, 640, 460),
            Box::new(super::powerbox::Panel::new()),
        ),
        // The Milk Drop incoming panel (Powerbox style, SPEC 644 wide).
        App::Drop => (
            Rect::new((super::theme::screen_w() - 644) / 2, 150, 644, 330),
            Box::new(milkdrop::Receiver::new()),
        ),
        App::Settings => (
            Rect::new(180, 80, 660, 468),
            Box::new(settings::Settings::new()),
        ),
        App::Client(id) => {
            let (w, h) = super::client::info(id).map_or((320, 240), |i| (i.0 as i32, i.1 as i32));
            // Body + frame (1px border each side, 32px title bar + its line).
            let r = super::client::place(id as i32 % 8, w + 2, h + 34);
            (r, super::client::ClientView::new(id))
        }
    }
}
