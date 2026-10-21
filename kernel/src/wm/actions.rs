//! App actions for Skim: anything an app can do that is worth one keystroke from anywhere
//! ("New note", "Use dark theme", "Back up now"). Apps register a name and a plain function;
//! Skim searches them next to apps, files and settings, and runs the chosen one on the wm
//! thread. Actions talk to the desktop the same way any kernel code can: `wm::request`,
//! `wm::notify`, the app modules' own entry points.
//!
//! ```ignore
//! wm::actions::register("Notes", "New note", new_note);
//! ```

use alloc::vec::Vec;

use crate::sync::SpinLock;

use super::apps::{browser, files, music, notes};
use super::prefs;
use super::window::App;
use super::{Mode, Request, request};

pub type ActionFn = fn();

#[derive(Clone, Copy)]
pub struct Action {
    /// The app it belongs to (shown under the name, and searched too).
    pub app: &'static str,
    pub name: &'static str,
    pub run: ActionFn,
}

static ACTIONS: SpinLock<Vec<Action>> = SpinLock::new(Vec::new());

/// Register an action. Registering the same (app, name) again replaces it.
pub fn register(app: &'static str, name: &'static str, run: ActionFn) {
    let mut a = ACTIONS.lock();
    a.retain(|x| !(x.app == app && x.name == name));
    a.push(Action { app, name, run });
}

pub fn all() -> Vec<Action> {
    ACTIONS.lock().clone()
}

// ---- the built-in apps' actions --------------------------------------------------------------

fn new_note() {
    // ~/Documents/Note N.md, the first N that is free.
    let mut n = 1;
    let path = loop {
        let p = alloc::format!("/home/milk/Documents/Note {n}.md");
        if crate::fs::stat(&p).is_err() {
            break p;
        }
        n += 1;
    };
    let _ = crate::fs::write_file(&path, alloc::format!("# Note {n}\n").as_bytes());
    notes::open_file(&path);
    request(Request::OpenHere(App::Notes));
}

fn set_dark(on: bool) {
    let mut p = prefs::get();
    p.dark = on;
    request(Request::Prefs(p));
}

fn back_up_now() {
    let ok = super::apps::fsx::ctl_write("/sys/fs/milkfs/auto", "now").is_ok();
    super::notify(
        "Time Machine",
        if ok {
            "Backing up now"
        } else {
            "No milkfs disk to back up to"
        },
    );
}

fn go(path: &'static str) {
    files::show_path(path);
    request(Request::Open(App::Files));
}

/// The actions of the apps that ship with the desktop.
pub fn register_builtin() {
    register("Notes", "New note", new_note);
    register("Terminal", "New Terminal tab", || {
        request(Request::NewTab(None))
    });
    register("Files", "Go to Downloads", || go("/home/milk/Downloads"));
    register("Files", "Go to Documents", || go("/home/milk/Documents"));
    register("Files", "Show Time Machine", || go("timemachine:"));
    register("Settings", "Use dark theme (Chocolate milk)", || {
        set_dark(true)
    });
    register("Settings", "Use light theme (Whole milk)", || {
        set_dark(false)
    });
    register("Time Machine", "Back up now", back_up_now);
    register("Desktop", "Lock screen", || request(Request::Lock));
    register("Desktop", "Show Today", || {
        request(Request::Mode(Mode::Home))
    });
    register("Desktop", "Show Spaces", || {
        request(Request::Mode(Mode::Spaces))
    });
    register("Browser", "Open the milkOS home page", || {
        browser::open_url(browser::HOME);
        request(Request::Open(App::Browser));
    });
    register("Music", "Play / pause music", || {
        if music::now().track.is_some() {
            music::toggle();
        } else {
            music::play_path("/home/milk/Music/morning-pasture.wav");
            request(Request::Open(App::Music));
        }
    });
    register("Paint", "New painting", || {
        request(Request::OpenAny(alloc::string::String::from("paint")))
    });
}
