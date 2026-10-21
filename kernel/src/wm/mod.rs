//! The milkOS window system: compositor, window manager and shell (SPEC: docs/ui-concepts/SPEC.md).
//!
//! One [`Desktop`] owns the screen. Layers, bottom to top:
//! wallpaper -> windows (z-order) -> shell panels (menu bar, dock, toasts, control centre, launcher,
//! home cards, spaces overview) -> cursor.
//!
//! * Every window draws into its own offscreen [`Buffer`]; the compositor never asks a window to
//!   draw just because something moved over it.
//! * Shell panels are rendered once into small cached buffers (glass background baked in) and
//!   re-rendered only when their content changes.
//! * Changes add rectangles to a `gfx::DirtyList`; each frame composites only those rectangles
//!   into the back buffer and copies them to the ramfb framebuffer. Inside a damaged rect,
//!   everything under the topmost window that fully covers it is skipped.
//!
//! Threading: today [`run`] is the kmain loop (poll input, update, composite, `wfi`). Nothing here
//! assumes it runs on the boot stack, so it can become a kernel thread once `proc` lands: put
//! `Desktop::frame` in that thread's loop and replace the `wfi` with a sleep-until-event.

mod anim;
mod apps;
mod buffer;
pub mod client;
pub mod clipboard;
mod ctl;
mod event;
mod fontx;
mod icons;
mod menus;
mod paint;
mod power;
mod rtc;
mod script;
mod shell;
mod theme;
pub mod tty;
mod wallpaper;
mod window;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::gfx::{DirtyList, DrawTarget, Rect, Surface};
use crate::sched::{self, WaitQueue};
use crate::sync::SpinLock;
use crate::{cursor, display, input, kprintln, timer};

use buffer::Buffer;
pub use event::{Button, Event, Key};
use paint::{Painter, contains, hit};
pub use power::panic_screen;
use shell::{Layer, PanelKey, PanelSpec, Style};
use theme::*;
use wallpaper::Wallpaper;
use window::{App, KeyInput, Part, PointerInput, PointerKind, Window, WindowContents};

// ---- public API ------------------------------------------------------------------------------

/// Toasts posted from anywhere (any context but IRQ handlers: it allocates).
static NOTIFY: SpinLock<Vec<(String, String)>> = SpinLock::new(Vec::new());

/// Show a toast notification (SPEC: Toast) at the top right for a few seconds.
pub fn notify(title: &str, subtitle: &str) {
    NOTIFY
        .lock()
        .push((String::from(title), String::from(subtitle)));
    WAKE.wake_all();
}

/// The wm thread sleeps here between frames; input, toasts and requests wake it early.
static WAKE: WaitQueue = WaitQueue::new();

fn on_input(_ev: &input::Event) {
    WAKE.wake_all();
}

/// Frame period at 60 Hz.
const FRAME_NS: u64 = 16_666_667;
/// How long to sleep when nothing is moving (clocks, log followers and toasts still update).
const IDLE_NS: u64 = 50_000_000;

/// Things kernel code (and the built-in apps) can ask the desktop to do.
pub enum Request {
    Open(App),
    /// Open, and if the app's window is on another space, bring it to this one.
    OpenHere(App),
    Mode(Mode),
    /// A process created a window (`client::create`).
    ClientCreate(u32),
    /// A process destroyed its window (or exited).
    ClientGone(u32),
    /// `open` from /dev/wm: an app name or an absolute path.
    OpenAny(String),
    Skim,
}

static REQUESTS: SpinLock<Vec<Request>> = SpinLock::new(Vec::new());

/// Queue a request; it is handled at the start of the next frame.
pub fn request(r: Request) {
    REQUESTS.lock().push(r);
    WAKE.wake_all();
}

static STATS: SpinLock<Stats> = SpinLock::new(Stats {
    frames: 0,
    last_us: 0,
    last_px: 0,
    full_us: 0,
    drag_avg_us: 0,
    drag_max_us: 0,
    drag_frames: 0,
    cpu_pct: 0,
});

/// The compositor's frame statistics as of the last frame.
pub fn stats() -> Stats {
    *STATS.lock()
}

/// Take over the screen and run the desktop forever. Runs as the "wm" kernel thread (see
/// kmain): it sleeps between frames on a wait queue that input, toasts and requests kick, so it
/// never spins. Falls back to the text console if there is no framebuffer or not enough memory.
pub fn run() {
    let Some(fb) = display::take_framebuffer() else {
        kprintln!("wm: no framebuffer, no desktop");
        return;
    };
    let t0 = timer::now_ns();
    let mut d = match Desktop::new(fb) {
        Ok(d) => d,
        Err(fb) => {
            kprintln!("wm: out of memory for the desktop, text console instead");
            display::console_fallback(fb);
            return;
        }
    };
    kprintln!(
        "wm: desktop up in {} ms (wallpaper + blur + first frame + fade)",
        (timer::now_ns() - t0) / 1_000_000
    );
    if input::subscribe(on_input).is_err() {
        kprintln!("wm: no input subscriber slot, polling at 60 Hz");
    }
    let mut next = timer::now_ns();
    loop {
        let busy = d.frame();
        let now = timer::now_ns();
        // 60 Hz while something moves, a slow tick when idle; input wakes us at once.
        next = if busy {
            (next + FRAME_NS).max(now + FRAME_NS / 4)
        } else {
            now + IDLE_NS
        };
        WAKE.wait_until_timeout(
            || input::pending() > 0 || !NOTIFY.lock().is_empty() || !REQUESTS.lock().is_empty(),
            next.saturating_sub(now),
        );
    }
}

/// CPU time of the scheduler's idle thread, for the busy percentage.
fn idle_thread_ns() -> u64 {
    sched::threads()
        .iter()
        .filter(|t| t.name.as_str() == "idle")
        .map(|t| t.cpu_ns)
        .sum()
}

// ---- the desktop -----------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Desktop,
    /// Today: the wallpaper becomes a glanceable home of cards; windows are hidden.
    Home,
    /// Spaces overview.
    Spaces,
}

#[derive(Clone, Copy, Debug)]
enum Drag {
    Move {
        id: u32,
        dx: i32,
        dy: i32,
    },
    Resize {
        id: u32,
        ox: i32,
        oy: i32,
        w0: i32,
        h0: i32,
    },
    /// Pressed a traffic light; acts on release if still over it.
    Light {
        id: u32,
        which: usize,
    },
    /// Button went down in a window body: moves go to the client as drags.
    Content {
        id: u32,
    },
    /// Dragging a control centre slider.
    Slider(usize),
    /// Dragging a window cell in the Spaces overview.
    Cell {
        id: u32,
    },
    /// Pressed on something that acts on press only.
    Inert,
}

struct CachedPanel {
    rect: Rect,
    solid: bool,
    buf: Buffer,
}

/// Frame cost statistics (for the log and the About window).
#[derive(Default, Clone, Copy)]
pub struct Stats {
    pub frames: u64,
    pub last_us: u64,
    pub last_px: u64,
    /// Full-screen composite + flush, measured at startup.
    pub full_us: u64,
    /// Exponential average of the frame cost while dragging a window, in microseconds.
    pub drag_avg_us: u64,
    pub drag_max_us: u64,
    pub drag_frames: u64,
    /// Busy percentage over the last second (time not spent in `wfi`).
    pub cpu_pct: u32,
}

pub struct Settings {
    /// Focus mode: toasts are held back.
    pub focus_mode: bool,
    /// Display brightness 20..=100.
    pub brightness: i32,
    pub volume: i32,
}

pub struct Desktop {
    fb: Surface<'static>,
    back: Buffer,
    wp: Wallpaper,
    windows: Vec<Window>,
    focus: Option<u32>,
    next_id: u32,
    space: usize,
    nspaces: usize,
    mode: Mode,
    launcher: Option<shell::Launcher>,
    control: bool,
    toasts: Vec<shell::Toast>,
    next_toast: u32,
    settings: Settings,
    cache: BTreeMap<PanelKey, CachedPanel>,
    /// Running window animations (anim.rs).
    anims: Vec<anim::Anim>,
    /// Signature of the full-screen layers (dims) last composited.
    dims: Vec<u8>,
    /// Spaces overview thumbnails, keyed by window id.
    thumbs: BTreeMap<u32, Buffer>,
    pointer: (i32, i32),
    drag: Option<Drag>,
    /// Window whose traffic lights the pointer is over.
    hover_lights: Option<u32>,
    /// Dock item under the pointer (its name shows above it).
    tip: Option<usize>,
    /// Dock item being pressed (drawn 4% darker until release).
    dock_pressed: Option<usize>,
    /// Open menu bar menu (index into `menu_titles`) and the row under the pointer.
    menu: Option<usize>,
    menu_hover: Option<usize>,
    damage: DirtyList<32>,
    pub stats: Stats,
    status: String,
    script: Option<script::Script>,
    /// Compose into the back buffer only (the boot fade-in does the first flush itself).
    hold_flush: bool,
    // CPU accounting: the idle thread's CPU time at the start of the last second.
    idle_cpu_ns: u64,
    acct_start: u64,
    last_second: u64,
}

impl Desktop {
    fn new(fb: Surface<'static>) -> Result<Desktop, Surface<'static>> {
        rtc::init();
        let Some(back) = Buffer::new(SCREEN_W as usize, SCREEN_H as usize, 0) else {
            return Err(fb);
        };
        let t = timer::now_ns();
        let Some(wp) = Wallpaper::new() else {
            return Err(fb);
        };
        kprintln!(
            "wm: wallpaper W + glass Wb (box blur r{} x{}) in {} ms",
            BLUR_RADIUS,
            BLUR_PASSES,
            (timer::now_ns() - t) / 1_000_000
        );
        let mut d = Desktop {
            fb,
            back,
            wp,
            windows: Vec::new(),
            focus: None,
            next_id: 1,
            space: 0,
            nspaces: 3,
            mode: Mode::Desktop,
            launcher: None,
            control: false,
            toasts: Vec::new(),
            next_toast: 1,
            settings: Settings {
                focus_mode: false,
                brightness: 100,
                volume: 40,
            },
            cache: BTreeMap::new(),
            anims: Vec::new(),
            dims: Vec::new(),
            thumbs: BTreeMap::new(),
            pointer: input::pointer(),
            drag: None,
            hover_lights: None,
            tip: None,
            dock_pressed: None,
            menu: None,
            menu_hover: None,
            damage: DirtyList::new(),
            stats: Stats::default(),
            status: String::new(),
            script: None,
            hold_flush: true,
            idle_cpu_ns: idle_thread_ns(),
            acct_start: timer::now_ns(),
            last_second: timer::now_ns() / 1_000_000_000,
        };
        d.status = d.status_text();
        apps::fsx::ensure_home();
        ctl::init();
        clipboard::init();
        // The mockup's desktop: Files behind, Terminal in front and focused.
        d.open(App::Files);
        d.open(App::Terminal);
        // Something on the second space for the overview to show.
        d.space = 1;
        d.open(App::Notes);
        d.space = 0;
        d.focus_top();
        let demo = script::from_fw_cfg();
        if demo.is_none() {
            d.post_toast("Welcome to milkOS", "Ctrl+Space: search");
        }
        d.script = demo;

        // First frame: full screen, timed.
        d.damage_all();
        let t = timer::now_ns();
        d.frame();
        d.stats.full_us = (timer::now_ns() - t) / 1000;
        kprintln!(
            "wm: first full-screen composite {} us ({} px)",
            d.stats.full_us,
            d.stats.last_px
        );
        d.fade_in();
        Ok(d)
    }

    /// Cross-fade from whatever is on screen (the boot splash) to the composed desktop.
    fn fade_in(&mut self) {
        let n = (SCREEN_W * SCREEN_H) as usize;
        let splash: Option<Buffer> = Buffer::new(SCREEN_W as usize, SCREEN_H as usize, 0);
        if let Some(mut sp) = splash {
            sp.pixels_mut()[..n].copy_from_slice(&self.fb.pixels()[..n]);
            const STEPS: u32 = 12;
            for step in 1..STEPS {
                let a = (step * 255 / STEPS) as u8;
                let (src, back) = (sp.pixels(), self.back.pixels());
                for (i, p) in self.fb.pixels_mut()[..n].iter_mut().enumerate() {
                    *p = crate::gfx::blend(src[i], back[i], a);
                }
                sched::sleep_ms(14);
            }
        }
        self.hold_flush = false;
        self.fb.pixels_mut()[..n].copy_from_slice(&self.back.pixels()[..n]);
    }

    // ---- damage -------------------------------------------------------------------------------

    fn damage(&mut self, r: Rect) {
        let r = r.clipped(SCREEN_W as usize, SCREEN_H as usize);
        if !r.is_empty() {
            self.damage.add(r);
        }
    }

    fn damage_all(&mut self) {
        self.damage(Rect::new(0, 0, SCREEN_W, SCREEN_H));
    }

    fn cursor_rect(&self) -> Rect {
        Rect::new(
            self.pointer.0 - cursor::HOTSPOT.0,
            self.pointer.1 - cursor::HOTSPOT.1,
            cursor::SPRITE_W as i32,
            cursor::SPRITE_H as i32,
        )
    }

    // ---- windows --------------------------------------------------------------------------------

    fn win(&self, id: u32) -> Option<&Window> {
        self.windows.iter().find(|w| w.id == id)
    }

    fn win_mut(&mut self, id: u32) -> Option<&mut Window> {
        self.windows.iter_mut().find(|w| w.id == id)
    }

    fn idx(&self, id: u32) -> Option<usize> {
        self.windows.iter().position(|w| w.id == id)
    }

    fn visible(&self, w: &Window) -> bool {
        self.mode == Mode::Desktop && !w.minimized && w.space == self.space
    }

    fn focused_window(&self) -> Option<&Window> {
        self.focus.and_then(|id| self.win(id))
    }

    /// Open `app`, or bring its window forward if it is already running (single instance).
    fn open(&mut self, app: App) {
        if let Some(id) = self.windows.iter().find(|w| w.app == app).map(|w| w.id) {
            self.show_window(id);
            return;
        }
        let (rect, contents): (Rect, Box<dyn WindowContents>) = apps::create(app);
        let id = self.next_id;
        self.next_id += 1;
        let Some(w) = Window::new(id, app, rect, self.space, contents) else {
            self.post_toast("Out of memory", app.name());
            return;
        };
        self.windows.push(w);
        self.invalidate(PanelKey::Dock);
        if self.mode != Mode::Desktop {
            self.set_mode(Mode::Desktop);
        }
        self.set_focus(Some(id));
        self.damage_window(id);
        if self.stats.frames > 0 {
            self.anim_open(id); // not for the windows the desktop starts with
        }
    }

    /// Start a userland program (kernel-parented; a small thread reaps it). If it already has
    /// a window, focus that instead.
    fn launch(&mut self, path: &str) {
        let name = path.rsplit('/').next().unwrap_or(path);
        if let Some(id) = self.program_window(name) {
            self.show_window(id);
            return;
        }
        match crate::proc::spawn(path, &[name]) {
            Ok(pid) => {
                kprintln!("wm: started {path} as pid {pid}");
                drop(sched::spawn("reap-app", move || {
                    crate::proc::wait_kernel_child(pid);
                }));
            }
            Err(e) => self.post_toast("Can't start", &alloc::format!("{path}: errno {}", e.0)),
        }
    }

    /// `open` from /dev/wm: an app by name, a folder (Files), a text file (Notes) or a program.
    fn open_any(&mut self, what: &str) {
        match what.to_ascii_lowercase().as_str() {
            "files" | "finder" => return self.open(App::Files),
            "terminal" | "term" | "sh" | "msh" => return self.open(App::Terminal),
            "notes" => return self.open(App::Notes),
            "about" | "about milkos" => return self.open(App::About),
            "today" | "home" => return self.toggle_mode(Mode::Home),
            "spaces" => return self.toggle_mode(Mode::Spaces),
            "paint" => return self.launch("/bin/paint"),
            _ => {}
        }
        let path = apps::fsx::resolve("/", what);
        if apps::fsx::is_dir(&path) {
            apps::files::show_path(&path);
            self.open(App::Files);
        } else if path.starts_with("/bin/") && crate::fs::stat(&path).is_ok() {
            self.launch(&path);
        } else if crate::fs::stat(&path).is_ok() && apps::fsx::is_text(&path) {
            apps::notes::open_file(&path);
            let sp = self.space;
            if let Some(w) = self.windows.iter_mut().find(|w| w.app == App::Notes) {
                w.space = sp;
            }
            self.open(App::Notes);
        } else {
            self.post_toast("Can't open", what);
        }
    }

    /// The /dev/wm status text.
    fn status_report(&self) -> String {
        let mode = match self.mode {
            Mode::Desktop => "desktop",
            Mode::Home => "today",
            Mode::Spaces => "spaces",
        };
        let mut s = alloc::format!(
            "mode {mode}\nspace {} of {}\nfocus {}\n",
            self.space + 1,
            self.nspaces,
            self.focused_window().map_or("none", |w| w.contents.title())
        );
        for w in &self.windows {
            let r = w.rect;
            s.push_str(&alloc::format!(
                "window {} {}x{}+{}+{} space {}{} {}\n",
                w.id,
                r.w,
                r.h,
                r.x,
                r.y,
                w.space + 1,
                if w.minimized { " minimised" } else { "" },
                w.contents.title()
            ));
        }
        s
    }

    /// A client window whose process is called `name`.
    fn program_window(&self, name: &str) -> Option<u32> {
        self.windows
            .iter()
            .find(|w| {
                matches!(w.app, App::Client(c) if client::process_name(c).as_deref() == Some(name))
            })
            .map(|w| w.id)
    }

    fn running_program(&self, path: &str) -> bool {
        self.program_window(path.rsplit('/').next().unwrap_or(path))
            .is_some()
    }

    /// Make a window visible and focused: unminimise, switch to its space, leave overlays.
    fn show_window(&mut self, id: u32) {
        let Some(w) = self.win_mut(id) else {
            return;
        };
        let was_minimised = w.minimized;
        w.minimized = false;
        let sp = w.space;
        if self.mode != Mode::Desktop {
            self.set_mode(Mode::Desktop);
        }
        if sp != self.space {
            self.switch_space(sp);
        }
        self.set_focus(Some(id));
        self.damage_window(id);
        if was_minimised {
            self.anim_restore(id);
        }
    }

    fn damage_window(&mut self, id: u32) {
        if let Some(b) = self.win(id).map(|w| w.bounds()) {
            self.damage(b);
        }
    }

    /// Focus (and raise) a window, or nothing.
    fn set_focus(&mut self, id: Option<u32>) {
        if let Some(id) = id
            && let Some(i) = self.idx(id)
        {
            let w = self.windows.remove(i);
            let b = w.bounds();
            self.windows.push(w);
            if i != self.windows.len() - 1 {
                self.damage(b);
            }
        }
        if self.focus != id {
            self.focus = id;
            self.invalidate(PanelKey::MenuBar);
        }
    }

    /// Focus the topmost visible window (after a close / minimise / space switch).
    fn focus_top(&mut self) {
        let top = self
            .windows
            .iter()
            .rev()
            .find(|w| !w.minimized && w.space == self.space)
            .map(|w| w.id);
        self.set_focus(top);
    }

    /// Close from the UI (traffic light, Cmd+W, menu). A client's window goes away at once and
    /// the process gets a Close event.
    fn close(&mut self, id: u32) {
        if let Some(App::Client(cid)) = self.win(id).map(|w| w.app) {
            client::closed(cid);
        }
        self.close_window(id);
    }

    fn close_window(&mut self, id: u32) {
        self.damage_window(id);
        if let Some(i) = self.idx(id) {
            let w = self.windows.remove(i);
            if self.visible(&w) {
                self.anim_close(w);
            }
        }
        self.thumbs.remove(&id);
        self.invalidate(PanelKey::Dock);
        if self.focus == Some(id) {
            self.focus = None;
            self.focus_top();
            self.invalidate(PanelKey::MenuBar);
        }
    }

    fn minimise(&mut self, id: u32) {
        self.damage_window(id);
        self.anim_minimise(id);
        if let Some(w) = self.win_mut(id) {
            w.minimized = true;
        }
        if self.focus == Some(id) {
            self.focus = None;
            self.focus_top();
            self.invalidate(PanelKey::MenuBar);
        }
    }

    /// The area between the menu bar and the dock.
    fn work_area() -> Rect {
        let dock_top = SCREEN_H - DOCK_BOTTOM - DOCK_H;
        Rect::new(0, MENU_H, SCREEN_W, dock_top - MENU_H)
    }

    fn zoom(&mut self, id: u32) {
        self.damage_window(id);
        if let Some(w) = self.win_mut(id) {
            let r = match w.restore.take() {
                Some(r) => r,
                None => {
                    w.restore = Some(w.rect);
                    Self::work_area()
                }
            };
            w.set_rect(r);
        }
        self.damage_window(id);
    }

    fn move_window(&mut self, id: u32, x: i32, y: i32) {
        let Some(w) = self.win(id) else {
            return;
        };
        let old = w.bounds();
        let (ww, _) = (w.rect.w, w.rect.h);
        // Keep the title bar reachable.
        let x = x.clamp(80 - ww, SCREEN_W - 80);
        let y = y.clamp(MENU_H, SCREEN_H - TITLE_H - 8);
        let w = self.win_mut(id).unwrap();
        if (w.rect.x, w.rect.y) == (x, y) {
            return;
        }
        w.rect.x = x;
        w.rect.y = y;
        w.restore = None;
        let new = w.bounds();
        self.damage(old);
        self.damage(new);
    }

    fn resize_window(&mut self, id: u32, nw: i32, nh: i32) {
        let Some(w) = self.win(id) else {
            return;
        };
        let r = w.rect;
        let nw = nw.clamp(WIN_MIN_W, SCREEN_W - r.x.max(0));
        let nh = nh.clamp(WIN_MIN_H, SCREEN_H - r.y);
        if (nw, nh) == (r.w, r.h) {
            return;
        }
        self.damage_window(id);
        let w = self.win_mut(id).unwrap();
        w.set_rect(Rect::new(r.x, r.y, nw, nh));
        w.restore = None;
        self.damage_window(id);
    }

    fn switch_space(&mut self, sp: usize) {
        if sp >= self.nspaces || sp == self.space {
            return;
        }
        self.space = sp;
        self.focus_top();
        self.invalidate(PanelKey::MenuBar);
        self.damage_all();
    }

    fn set_mode(&mut self, m: Mode) {
        if self.mode == m {
            return;
        }
        self.mode = m;
        self.drag = None;
        if m == Mode::Spaces {
            self.build_thumbs();
        } else {
            self.thumbs.clear();
        }
        self.invalidate(PanelKey::MenuBar);
        self.invalidate(PanelKey::Dock);
        self.damage_all();
    }

    /// Downscale every window's surface for the overview (cells and the space strip).
    fn build_thumbs(&mut self) {
        self.thumbs.clear();
        for w in &mut self.windows {
            let f = Some(w.id) == self.focus;
            w.render(f);
        }
        for w in &self.windows {
            let (tw, th) = shell::cell_size(w.rect.w, w.rect.h);
            if let Some(mut b) = Buffer::new(tw as usize, th as usize, MILK) {
                paint::downscale(&w.surf, &mut b);
                self.thumbs.insert(w.id, b);
            }
        }
    }

    // ---- panels ---------------------------------------------------------------------------------

    /// Drop a panel's cached rendering; it is re-rendered (and its area recomposited) next frame.
    fn invalidate(&mut self, key: PanelKey) {
        if let Some(c) = self.cache.remove(&key) {
            self.damage(shell::panel_bounds(c.rect));
        }
    }

    fn post_toast(&mut self, title: &str, sub: &str) {
        if self.settings.focus_mode {
            kprintln!("wm: toast held back (focus mode): {title} / {sub}");
            return;
        }
        let id = self.next_toast;
        self.next_toast += 1;
        self.toasts.push(shell::Toast {
            id,
            title: String::from(title),
            sub: String::from(sub),
            until_ns: timer::now_ns() + 5_000_000_000,
        });
        if self.toasts.len() > 3 {
            self.toasts.remove(0);
        }
        // Positions shift: re-render them all.
        let ids: Vec<u32> = self.toasts.iter().map(|t| t.id).collect();
        for i in ids {
            self.invalidate(PanelKey::Toast(i));
        }
    }

    fn render_panel(&self, spec: &PanelSpec, solid: bool) -> Option<Buffer> {
        let r = spec.rect;
        let mut buf = Buffer::new(r.w as usize, r.h as usize, MILK)?;
        {
            let mut p = Painter::new(&mut buf);
            match spec.style {
                Style::Glass | Style::GlassStrong | Style::Bar if !solid => {
                    let g = if spec.style == Style::GlassStrong {
                        &self.wp.glass_strong
                    } else {
                        &self.wp.glass
                    };
                    p.blit(0, 0, g, r.x, r.y, r.w, r.h);
                }
                Style::Glass | Style::GlassStrong | Style::Bar => p.fill(0, 0, r.w, r.h, MILK),
                Style::Opaque => {}
            }
            match spec.style {
                Style::Glass | Style::GlassStrong => {
                    p.rrect_stroke(0, 0, r.w, r.h, spec.radius, 1, WHITE, GLASS_BORDER_A)
                }
                Style::Bar => p.fill_a(0, r.h - 1, r.w, 1, WHITE, GLASS_BORDER_A),
                Style::Opaque => {}
            }
            self.paint_panel(spec.key, &mut p, r.w, r.h, solid);
        }
        Some(buf)
    }

    /// Bring the panel cache in line with the current layers, damaging whatever changed.
    fn reconcile(&mut self, layers: &[Layer]) {
        let dims: Vec<u8> = layers
            .iter()
            .filter_map(|l| match l {
                Layer::Dim(a) => Some(*a),
                _ => None,
            })
            .collect();
        if dims != self.dims {
            self.dims = dims;
            self.damage_all();
        }
        let vis: Vec<Rect> = self
            .windows
            .iter()
            .filter(|w| self.visible(w))
            .map(|w| w.rect)
            .collect();
        let mut live: Vec<PanelKey> = Vec::new();
        for l in layers {
            let Layer::Panel(spec) = l else {
                continue;
            };
            live.push(spec.key);
            let solid = spec.style != Style::Opaque
                && spec.style != Style::Bar
                && vis.iter().any(|w| w.intersects(&spec.rect));
            let ok = self
                .cache
                .get(&spec.key)
                .is_some_and(|c| c.rect == spec.rect && c.solid == solid);
            if ok {
                continue;
            }
            if let Some(c) = self.cache.remove(&spec.key) {
                self.damage(shell::panel_bounds(c.rect));
            }
            if let Some(buf) = self.render_panel(spec, solid) {
                self.cache.insert(
                    spec.key,
                    CachedPanel {
                        rect: spec.rect,
                        solid,
                        buf,
                    },
                );
            }
            self.damage(shell::panel_bounds(spec.rect));
        }
        let stale: Vec<PanelKey> = self
            .cache
            .keys()
            .filter(|k| !live.contains(k))
            .copied()
            .collect();
        for k in stale {
            self.invalidate(k);
        }
    }

    // ---- compositing ----------------------------------------------------------------------------

    /// Composite every damaged rect into the back buffer and copy it to the screen.
    fn compose(&mut self, layers: &[Layer]) {
        if self.damage.is_empty() {
            return;
        }
        let t0 = timer::now_ns();
        let rects: Vec<Rect> = self.damage.rects().to_vec();
        self.damage.clear();
        let mut px = 0u64;
        let vis: Vec<usize> = (0..self.windows.len())
            .filter(|&i| self.visible(&self.windows[i]) && !self.animating(self.windows[i].id))
            .collect();
        let cur = self.cursor_rect();
        for d in &rects {
            px += (d.w * d.h) as u64;
            self.compose_rect(*d, layers, &vis, cur);
        }
        // Flush to ramfb.
        if !self.hold_flush {
            let fstride = self.fb.stride();
            let bstride = self.back.stride();
            let src = self.back.pixels();
            let dst = self.fb.pixels_mut();
            for d in &rects {
                for y in d.y..d.y + d.h {
                    let (s, f) = (y as usize * bstride, y as usize * fstride);
                    let (x0, x1) = (d.x as usize, (d.x + d.w) as usize);
                    dst[f + x0..f + x1].copy_from_slice(&src[s + x0..s + x1]);
                }
            }
        }
        let us = (timer::now_ns() - t0) / 1000;
        self.stats.frames += 1;
        self.stats.last_us = us;
        self.stats.last_px = px;
        if matches!(self.drag, Some(Drag::Move { .. })) {
            let s = &mut self.stats;
            s.drag_frames += 1;
            s.drag_max_us = s.drag_max_us.max(us);
            s.drag_avg_us = if s.drag_avg_us == 0 {
                us
            } else {
                (s.drag_avg_us * 7 + us) / 8
            };
        }
    }

    fn compose_rect(&mut self, d: Rect, layers: &[Layer], vis: &[usize], cur: Rect) {
        let mut p = Painter::new(&mut self.back);
        let mut p = p.clipped(d);
        // Skip everything under the topmost window that covers the whole rect.
        let start = vis
            .iter()
            .rposition(|&i| contains(&self.windows[i].core(), &d));
        match start {
            Some(_) => {}
            None => p.blit_same(d, &self.wp.sharp),
        }
        for (k, &i) in vis.iter().enumerate() {
            if start.is_some_and(|s| k < s) {
                continue;
            }
            let w = &self.windows[i];
            if !w.bounds().intersects(&d) {
                continue;
            }
            if start != Some(k) {
                p.shadow_window(w.rect);
            }
            p.blit_rrect(
                w.rect.x, w.rect.y, &w.surf, 0, 0, w.rect.w, w.rect.h, R_WINDOW, 255,
            );
        }
        if !self.anims.is_empty() {
            anim::draw(&self.anims, &self.windows, &mut p, d);
        }
        for l in layers {
            match l {
                Layer::Dim(a) => p.fill_a(d.x, d.y, d.w, d.h, ESPRESSO, *a),
                Layer::Label {
                    x,
                    y,
                    w,
                    text,
                    color,
                } => {
                    if Rect::new(*x, *y, *w, 16).intersects(&d) {
                        p.text_center(*x, *w, *y, text, *color, true);
                    }
                }
                Layer::Panel(spec) => {
                    if !shell::panel_bounds(spec.rect).intersects(&d) {
                        continue;
                    }
                    let Some(c) = self.cache.get(&spec.key) else {
                        continue;
                    };
                    if spec.shadow {
                        p.shadow(spec.rect, spec.radius, &SHADOW_FLOAT);
                    }
                    let a = if c.solid { GLASS_SOLID_A } else { 255 };
                    let r = spec.rect;
                    p.blit_rrect(r.x, r.y, &c.buf, 0, 0, r.w, r.h, spec.radius, a);
                }
            }
        }
        if self.settings.brightness < 100 {
            let a = ((100 - self.settings.brightness) * 255 / 100 * 7 / 10) as u8;
            p.fill_a(d.x, d.y, d.w, d.h, 0, a);
        }
        if cur.intersects(&d) {
            p.sprite(
                cur.x,
                cur.y,
                &cursor::SPRITE,
                cursor::SPRITE_W,
                cursor::SPRITE_H,
            );
        }
    }

    // ---- the frame loop -------------------------------------------------------------------------

    /// One iteration: take input, update, render, composite. True if anything changed (or a
    /// drag or demo script is running), so the next frame should come at the full 60 Hz.
    pub fn frame(&mut self) -> bool {
        let now = timer::now_ns();
        let frames = self.stats.frames;
        // Input: virtio input, then the demo script.
        while let Some(ev) = input::poll() {
            if let Some(ev) = event::from_input(ev) {
                self.handle(ev);
            }
        }
        if let Some(mut s) = self.script.take() {
            let done = s.step(self, now);
            if !done {
                self.script = Some(s);
            }
        }
        let posted: Vec<(String, String)> = core::mem::take(&mut *NOTIFY.lock());
        for (t, s) in posted {
            self.post_toast(&t, &s);
        }
        let reqs: Vec<Request> = core::mem::take(&mut *REQUESTS.lock());
        for r in reqs {
            match r {
                Request::Open(a) => self.open(a),
                Request::OpenHere(a) => {
                    let sp = self.space;
                    if let Some(w) = self.windows.iter_mut().find(|w| w.app == a) {
                        w.space = sp;
                    }
                    self.open(a);
                }
                Request::Mode(m) => self.toggle_mode(m),
                Request::ClientCreate(id) => self.open(App::Client(id)),
                Request::OpenAny(what) => self.open_any(&what),
                Request::Skim => {
                    if self.launcher.is_none() {
                        self.toggle_launcher();
                    }
                }
                Request::ClientGone(id) => {
                    if let Some(w) = self.windows.iter().find(|w| w.app == App::Client(id)) {
                        let wid = w.id;
                        self.close_window(wid);
                    }
                }
            }
        }
        self.tick(now);
        let animating = self.step_anims(now);
        self.render_and_compose();
        *STATS.lock() = self.stats;
        self.stats.frames != frames
            || animating
            || self.script.is_some()
            || matches!(
                self.drag,
                Some(Drag::Move { .. } | Drag::Resize { .. } | Drag::Slider(_))
            )
    }

    /// Re-render stale window surfaces and panels, then composite the damage.
    fn render_and_compose(&mut self) {
        // Re-render windows whose surface is stale.
        let focus = self.focus;
        let (desk, space) = (self.mode == Mode::Desktop, self.space);
        let mut redrawn: Vec<Rect> = Vec::new();
        for w in &mut self.windows {
            if desk && !w.minimized && w.space == space && w.render(focus == Some(w.id)) {
                redrawn.push(w.rect);
            }
        }
        for r in redrawn {
            self.damage(r);
        }
        let layers = self.layers();
        self.reconcile(&layers);
        self.compose(&layers);
    }

    /// Measure frame costs: full-screen redraws, then a scripted window drag. Logs the results.
    fn bench(&mut self) {
        let mut full = Vec::new();
        for _ in 0..10 {
            self.damage_all();
            let t = timer::now_ns();
            self.render_and_compose();
            full.push((timer::now_ns() - t) / 1000);
        }
        let report = |name: &str, v: &mut Vec<u64>, px: u64| {
            v.sort_unstable();
            let avg = v.iter().sum::<u64>() / v.len().max(1) as u64;
            kprintln!(
                "wm: bench {name}: {} frames, min {} us, median {} us, avg {} us, max {} us ({} px/frame)",
                v.len(),
                v[0],
                v[v.len() / 2],
                avg,
                v[v.len() - 1],
                px
            );
        };
        let px = self.stats.last_px;
        report("full-screen", &mut full, px);
        // Drag the top window by its title bar, 3 px right and 1 px down per frame.
        let Some(id) = self.focus.or(self.windows.last().map(|w| w.id)) else {
            return;
        };
        let r = self.win(id).unwrap().rect;
        let (mut x, mut y) = (r.x + r.w / 2, r.y + 12);
        self.handle(Event::PointerMove { x, y });
        self.render_and_compose();
        self.handle(Event::Button {
            button: Button::Left,
            down: true,
        });
        self.render_and_compose();
        let mut drag = Vec::new();
        let mut pxs = 0;
        for i in 0..120 {
            let (dx, dy) = if i < 60 { (3, 1) } else { (-3, -1) };
            x += dx;
            y += dy;
            self.handle(Event::PointerMove { x, y });
            let t = timer::now_ns();
            self.render_and_compose();
            drag.push((timer::now_ns() - t) / 1000);
            pxs += self.stats.last_px;
        }
        self.handle(Event::Button {
            button: Button::Left,
            down: false,
        });
        self.render_and_compose();
        let n = drag.len() as u64;
        report("window drag", &mut drag, pxs / n);
        // Cursor only: a small move over a window.
        let mut cur = Vec::new();
        for i in 0..60 {
            self.handle(Event::PointerMove {
                x: x + (i % 2) * 2,
                y: y + 40,
            });
            let t = timer::now_ns();
            self.render_and_compose();
            cur.push((timer::now_ns() - t) / 1000);
        }
        let px = self.stats.last_px;
        report("cursor move", &mut cur, px);
    }

    fn tick(&mut self, now: u64) {
        // Expire toasts.
        let before = self.toasts.len();
        self.toasts.retain(|t| t.until_ns > now);
        if self.toasts.len() != before {
            let ids: Vec<u32> = self.toasts.iter().map(|t| t.id).collect();
            for i in ids {
                self.invalidate(PanelKey::Toast(i));
            }
        }
        // Clients.
        for w in &mut self.windows {
            if w.contents.tick(now) {
                w.dirty = true;
            }
        }
        // Once a second: CPU, clock, live panels.
        let sec = now / 1_000_000_000;
        if sec != self.last_second {
            self.last_second = sec;
            ctl::set_status(self.status_report());
            // Windows of userland clients that exited without destroying them.
            for cid in client::reap() {
                if let Some(wid) = self
                    .windows
                    .iter()
                    .find(|w| w.app == App::Client(cid))
                    .map(|w| w.id)
                {
                    self.close_window(wid);
                }
            }
            let span = now - self.acct_start;
            let idle = idle_thread_ns();
            if let Some(pct) = ((idle - self.idle_cpu_ns.min(idle)) * 100).checked_div(span) {
                self.stats.cpu_pct = (100 - pct.min(100)) as u32;
            }
            self.idle_cpu_ns = idle;
            self.acct_start = now;
            let st = self.status_text();
            if st != self.status {
                self.status = st;
                self.invalidate(PanelKey::MenuBar);
            }
            if self.mode == Mode::Home {
                for i in 0..6 {
                    self.invalidate(PanelKey::Card(i));
                }
            }
            if self.control {
                self.invalidate(PanelKey::Control);
            }
        }
    }

    // ---- input ----------------------------------------------------------------------------------

    pub fn handle(&mut self, ev: Event) {
        // A click or key press finishes running animations.
        if !self.anims.is_empty()
            && matches!(
                ev,
                Event::Button { down: true, .. } | Event::Key { down: true, .. }
            )
        {
            self.skip_anims();
        }
        match ev {
            Event::PointerMove { x, y } => self.pointer_move(x, y),
            Event::Button {
                button: Button::Left,
                down: true,
            } => self.press(),
            Event::Button {
                button: Button::Left,
                down: false,
            } => self.release(),
            Event::Button { .. } => {}
            Event::Scroll { dy } => {
                let (x, y) = self.pointer;
                if let Some(id) = self.window_at(x, y)
                    && let Some(w) = self.win_mut(id)
                    && w.contents.scroll(dy)
                {
                    w.dirty = true;
                }
            }
            Event::Key {
                key,
                ch,
                mods,
                down: true,
            } => self.key(KeyInput { key, ch, mods }),
            Event::Key { .. } => {}
        }
    }

    fn window_at(&self, x: i32, y: i32) -> Option<u32> {
        self.windows
            .iter()
            .rev()
            .find(|w| self.visible(w) && hit(&w.rect, x, y))
            .map(|w| w.id)
    }

    fn pointer_move(&mut self, x: i32, y: i32) {
        let (x, y) = (x.clamp(0, SCREEN_W - 1), y.clamp(0, SCREEN_H - 1));
        if (x, y) == self.pointer {
            return;
        }
        self.damage(self.cursor_rect());
        self.pointer = (x, y);
        self.damage(self.cursor_rect());
        match self.drag {
            Some(Drag::Move { id, dx, dy }) => self.move_window(id, x - dx, y - dy),
            Some(Drag::Resize { id, ox, oy, w0, h0 }) => {
                self.resize_window(id, w0 + x - ox, h0 + y - oy)
            }
            Some(Drag::Content { id }) => self.send_pointer(id, x, y, PointerKind::Drag),
            Some(Drag::Slider(i)) => self.slider_to(i, x),
            Some(_) => {}
            None => self.hover(x, y),
        }
    }

    /// Hover feedback: traffic-light symbols and the dock's name tips.
    fn hover(&mut self, x: i32, y: i32) {
        // Skim: the selection follows the pointer over the results.
        if self.launcher.is_some() {
            let rows = self.layers().iter().find_map(|l| match l {
                Layer::Panel(s) if s.key == PanelKey::LaunchResults => Some(s.rect),
                _ => None,
            });
            if let Some(r) = rows
                && hit(&r, x, y)
                && y - r.y >= 40
            {
                let row = ((y - r.y - 40) / LAUNCH_ROW_H) as usize;
                let n = self.launcher.as_ref().map_or(0, |l| l.result_count());
                if let Some(l) = &mut self.launcher
                    && row < n
                    && row != l.sel
                {
                    l.sel = row;
                    self.invalidate(PanelKey::LaunchResults);
                }
            }
        }
        if self.menu.is_some() {
            if y < MENU_H {
                if let Some(t) = self.menu_title_at(x) {
                    self.set_menu(Some(t));
                }
            } else if let Some(Layer::Panel(spec)) = self.menu_layer() {
                let row = if hit(&spec.rect, x, y) {
                    self.menu_row_at(y - spec.rect.y)
                } else {
                    None
                };
                if row != self.menu_hover {
                    self.menu_hover = row;
                    self.invalidate(PanelKey::Menu);
                }
            }
        }
        let hl = self.window_at(x, y).filter(|&id| {
            self.win(id)
                .is_some_and(|w| matches!(w.hit(x, y), Some(Part::Light(_))))
        });
        if hl != self.hover_lights {
            for id in [self.hover_lights, hl].into_iter().flatten() {
                if let Some(w) = self.win_mut(id) {
                    w.hover = Some(id) == hl;
                    w.dirty = true;
                }
            }
            self.hover_lights = hl;
        }
        let tip = if self.mode != Mode::Spaces && hit(&shell::dock_rect(), x, y) {
            shell::dock_index(x - shell::dock_rect().x)
        } else {
            None
        };
        if tip != self.tip {
            self.tip = tip;
            self.invalidate(PanelKey::Tip);
            self.invalidate(PanelKey::Dock);
        }
    }

    fn send_pointer(&mut self, id: u32, x: i32, y: i32, kind: PointerKind) {
        let Some(w) = self.win_mut(id) else {
            return;
        };
        let b = window::body_rect(w.rect.w, w.rect.h);
        let e = PointerInput {
            x: x - w.rect.x - b.x,
            y: y - w.rect.y - b.y,
            kind,
        };
        if w.contents.pointer(&e) {
            w.dirty = true;
        }
    }

    fn press(&mut self) {
        let (x, y) = self.pointer;
        self.drag = Some(Drag::Inert);
        // Shell layers first, topmost first.
        let layers = self.layers();
        for l in layers.iter().rev() {
            match l {
                Layer::Dim(_) => {
                    // A click on the dimmed area outside an overlay closes it.
                    if self.launcher.is_some() {
                        self.close_launcher();
                        return;
                    }
                }
                Layer::Panel(spec) if hit(&spec.rect, x, y) => {
                    let (lx, ly) = (x - spec.rect.x, y - spec.rect.y);
                    if self.menu.is_some()
                        && !matches!(spec.key, PanelKey::Menu | PanelKey::MenuBar)
                    {
                        self.set_menu(None);
                        return;
                    }
                    self.click_panel(spec.key, lx, ly);
                    return;
                }
                _ => {}
            }
        }
        if self.menu.is_some() {
            self.set_menu(None);
            return;
        }
        if self.control {
            self.control = false;
            return;
        }
        if self.mode != Mode::Desktop {
            if self.mode == Mode::Spaces {
                self.set_mode(Mode::Desktop);
            }
            return;
        }
        let Some(id) = self.window_at(x, y) else {
            return;
        };
        self.set_focus(Some(id));
        let w = self.win(id).unwrap();
        let r = w.rect;
        match w.hit(x, y) {
            Some(Part::Light(which)) => {
                self.drag = Some(Drag::Light { id, which });
                if let Some(w) = self.win_mut(id) {
                    w.pressed_light = Some(which);
                    w.dirty = true;
                }
            }
            Some(Part::Title) => {
                self.drag = Some(Drag::Move {
                    id,
                    dx: x - r.x,
                    dy: y - r.y,
                })
            }
            Some(Part::Resize) => {
                self.drag = Some(Drag::Resize {
                    id,
                    ox: x,
                    oy: y,
                    w0: r.w,
                    h0: r.h,
                })
            }
            Some(Part::Body) => {
                self.drag = Some(Drag::Content { id });
                self.send_pointer(id, x, y, PointerKind::Down(Button::Left));
            }
            None => {}
        }
    }

    fn release(&mut self) {
        let (x, y) = self.pointer;
        if self.dock_pressed.take().is_some() {
            self.invalidate(PanelKey::Dock);
        }
        match self.drag.take() {
            Some(Drag::Light { id, which }) => {
                if let Some(w) = self.win_mut(id) {
                    w.pressed_light = None;
                    w.dirty = true;
                }
                if let Some(w) = self.win(id)
                    && w.hit(x, y) == Some(Part::Light(which))
                {
                    match which {
                        0 => self.close(id),
                        1 => self.minimise(id),
                        _ => self.zoom(id),
                    }
                }
            }
            Some(Drag::Content { id }) => {
                self.send_pointer(id, x, y, PointerKind::Up(Button::Left))
            }
            Some(Drag::Cell { id }) => self.drop_cell(id, x, y),
            Some(Drag::Move { .. }) => {
                let s = self.stats;
                if s.drag_frames > 0 {
                    kprintln!(
                        "wm: drag frames {} avg {} us max {} us (last {} px)",
                        s.drag_frames,
                        s.drag_avg_us,
                        s.drag_max_us,
                        s.last_px
                    );
                }
            }
            _ => {}
        }
    }

    fn key(&mut self, k: KeyInput) {
        let m = k.mods;
        // Global shortcuts.
        match k.key {
            Key::Char(' ') if m.meta || m.ctrl => return self.toggle_launcher(),
            Key::Escape if self.menu.is_some() => return self.set_menu(None),
            Key::Escape if self.launcher.is_some() => return self.close_launcher(),
            Key::Escape if self.control => {
                self.control = false;
                return;
            }
            Key::Escape if self.mode != Mode::Desktop => return self.set_mode(Mode::Desktop),
            Key::F(3) => return self.toggle_mode(Mode::Spaces),
            Key::Up if m.ctrl => return self.toggle_mode(Mode::Spaces),
            Key::F(4) => return self.toggle_mode(Mode::Home),
            Key::Left if m.ctrl => return self.switch_space(self.space.saturating_sub(1)),
            Key::Right if m.ctrl => return self.switch_space(self.space + 1),
            _ => {}
        }
        if self.launcher.is_some() {
            return self.launcher_key(&k);
        }
        if m.meta {
            let mut handled = true;
            match k.key {
                Key::Char('w') => {
                    if let Some(id) = self.focus {
                        self.close(id);
                    }
                }
                Key::Char('m') => {
                    if let Some(id) = self.focus {
                        self.minimise(id);
                    }
                }
                Key::Tab | Key::Char('`') => self.cycle_windows(),
                // Everything else (Cmd+C / V / X / A ...) is the focused window's.
                _ => handled = false,
            }
            if handled {
                return;
            }
        }
        if self.mode != Mode::Desktop {
            return;
        }
        if let Some(id) = self.focus
            && let Some(w) = self.win_mut(id)
            && w.contents.key(&k)
        {
            w.dirty = true;
        }
    }

    /// Send the front window to the back of this space and focus the next one.
    fn cycle_windows(&mut self) {
        let vis: Vec<u32> = self
            .windows
            .iter()
            .filter(|w| !w.minimized && w.space == self.space)
            .map(|w| w.id)
            .collect();
        if vis.len() > 1
            && let Some(i) = self.idx(vis[vis.len() - 1])
        {
            let w = self.windows.remove(i);
            let b = w.bounds();
            self.windows.insert(0, w);
            self.damage(b);
            self.focus_top();
        }
    }

    fn set_menu(&mut self, m: Option<usize>) {
        if m != self.menu {
            self.menu = m;
            self.menu_hover = None;
            self.invalidate(PanelKey::Menu);
            self.invalidate(PanelKey::MenuBar);
        }
    }

    fn toggle_mode(&mut self, m: Mode) {
        if self.mode == m {
            self.set_mode(Mode::Desktop);
        } else {
            self.close_launcher();
            self.control = false;
            self.set_mode(m);
        }
    }
}
