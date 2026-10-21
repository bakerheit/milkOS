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

pub mod actions;
mod anim;
mod apps;
mod buffer;
pub mod client;
pub mod clipboard;
mod ctl;
mod dnd;
mod event;
mod fontx;
mod icons;
mod lock;
mod menus;
mod notif;
mod packages;
mod paint;
mod png;
mod power;
pub mod powerbox;
pub mod prefs;
mod rtc;
mod screen;
mod script;
pub mod session;
mod shell;
mod shot;
mod skim;
mod snap;
mod tabs;
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
/// Toasts posted from any thread: (app, title, subtitle, play the notify sound).
static NOTIFY: SpinLock<Vec<(String, String, String, bool)>> = SpinLock::new(Vec::new());

/// Who is posting: the calling process (e.g. /bin/notify), else the desktop itself.
fn poster() -> String {
    crate::proc::get(sched::current_pid())
        .filter(|_| sched::current_pid() != 0)
        .map_or(String::from("milkOS"), |p| {
            String::from(p.inner.lock().name.as_str())
        })
}

/// A toast on behalf of `app` (grouped under it in the notification centre).
pub fn notify_as(app: &str, title: &str, subtitle: &str) {
    NOTIFY.lock().push((
        String::from(app),
        String::from(title),
        String::from(subtitle),
        true,
    ));
    WAKE.wake_all();
}

/// Show a toast notification (SPEC: Toast) at the top right for a few seconds.
pub fn notify(title: &str, subtitle: &str) {
    notify_as(&poster(), title, subtitle);
}

/// A toast without the notify sound (when the action had its own, e.g. the shutter).
pub fn notify_quiet(app: &str, title: &str, subtitle: &str) {
    NOTIFY.lock().push((
        String::from(app),
        String::from(title),
        String::from(subtitle),
        false,
    ));
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

/// Set by the panic screen: the compositor stops flushing to the framebuffer (on SMP the wm
/// thread may still be running on another CPU).
pub static FROZEN: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

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
    /// New preferences from Settings: apply and save.
    Prefs(prefs::Prefs),
    Skim,
    /// A new Terminal tab, optionally running a command in it (Skim's `> cmd`).
    NewTab(Option<String>),
    /// Show the lock screen.
    Lock,
    /// Launch an installed package by name (sandboxed if it is a window app).
    OpenPackage(String),
    /// A powerbox request is waiting for its Open / Save panel (powerbox.rs).
    Powerbox,
    /// Nothing to do: just run a frame (a background job has a result for an app).
    Wake,
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
    // A display mode saved in Settings.
    let res = prefs::get().resolution;
    if res != (theme::screen_w(), theme::screen_h()) {
        d.set_resolution(res.0, res.1);
    }
    // First boot (until "Don't show again"): the Welcome tour. Not under demo scripts,
    // whose clicks assume the plain desktop.
    if prefs::get().welcome && !script::DEMO_BOOT.load(core::sync::atomic::Ordering::Relaxed) {
        d.open(App::Welcome);
    }
    if script::FIRSTBOOT.load(core::sync::atomic::Ordering::Relaxed) {
        kprintln!("[demo] firstboot done");
    }
    if input::subscribe(on_input).is_err() {
        kprintln!("wm: no input subscriber slot, polling at 60 Hz");
    }
    powerbox::set_up();
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
        /// Not moved yet (a snapped / zoomed window unsnaps on its first move).
        fresh: bool,
    },
    Resize {
        id: u32,
        ox: i32,
        oy: i32,
        w0: i32,
        h0: i32,
    },
    /// Pressed a traffic light; acts on release if still over it.
    Light { id: u32, which: usize },
    /// Button went down in a window body at (x0, y0): moves go to the client as drags,
    /// unless the first real move turns it into a file drag (`tried`: asked already).
    Content {
        id: u32,
        x0: i32,
        y0: i32,
        tried: bool,
    },
    /// Dragging a file (dnd.rs).
    File,
    /// Pressed on window tab `i` of `id` at (x0, y0): dragging it out of the strip makes it a
    /// window (tabs.rs).
    Tab { id: u32, i: usize, x0: i32, y0: i32 },
    /// Dragging a control centre slider.
    Slider(usize),
    /// Dragging a window cell in the Spaces overview.
    Cell { id: u32 },
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
    /// Snap preview while dragging a window to an edge (snap.rs).
    snap_preview: Option<Rect>,
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
    /// Window whose body the pointer is over (hover states).
    hover_body: Option<u32>,
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
    /// Busy % per online CPU (menu bar meter), and the same quantised.
    cpu_loads: Vec<u32>,
    cpu_steps: Vec<u32>,
    last_second: u64,
    /// The lock screen while it is up (or fading), lock.rs; its opacity this frame.
    lock: Option<lock::Lock>,
    lock_a: u8,
    /// Last key press or click (the idle lock timer); demos add pretend idle time.
    last_input: u64,
    idle_skew: u64,
    /// Cmd+Shift+4 region selection (shot.rs); compose without cursor / overlay (capture).
    shot: Option<shot::Select>,
    shot_hide: bool,
    /// A file being dragged (dnd.rs).
    dnd: Option<dnd::Dnd>,
    /// The "+" of this window's strip was pressed: the next app opened becomes its tab.
    add_tab_to: Option<u32>,
    /// The notification drawer is open, its state, and what it lists (notif.rs).
    notif: bool,
    drawer: notif::Drawer,
    history: Vec<notif::Notice>,
}

impl Desktop {
    fn new(fb: Surface<'static>) -> Result<Desktop, Surface<'static>> {
        rtc::init();
        let Some(back) = Buffer::new(screen_w() as usize, screen_h() as usize, 0) else {
            return Err(fb);
        };
        let t = timer::now_ns();
        let pr = prefs::load();
        theme::set_dark(pr.dark);
        theme::set_dock_icon(pr.dock);
        let Some(wp) = Wallpaper::new(pr.wallpaper_kind()) else {
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
                brightness: pr.brightness,
                volume: 40,
            },
            cache: BTreeMap::new(),
            anims: Vec::new(),
            snap_preview: None,
            lock: None,
            lock_a: 0,
            last_input: t,
            idle_skew: 0,
            shot: None,
            shot_hide: false,
            dnd: None,
            add_tab_to: None,
            notif: false,
            drawer: notif::Drawer::default(),
            history: Vec::new(),
            dims: Vec::new(),
            thumbs: BTreeMap::new(),
            pointer: input::pointer(),
            drag: None,
            hover_lights: None,
            hover_body: None,
            tip: None,
            dock_pressed: None,
            menu: None,
            menu_hover: None,
            damage: DirtyList::new(),
            stats: Stats::default(),
            status: String::new(),
            script: None,
            hold_flush: true,
            cpu_loads: Vec::new(),
            cpu_steps: Vec::new(),
            last_second: timer::now_ns() / 1_000_000_000,
        };
        d.status = d.status_text();
        apps::fsx::ensure_home();
        apps::music::seed_library();
        ctl::init();
        actions::register_builtin();
        clipboard::init();
        // The mockup's desktop: Files behind, Terminal in front and focused.
        d.open(App::Files);
        d.open(App::Terminal);
        // Something on the second space for the overview to show.
        d.space = 1;
        d.open(App::Notes);
        d.space = 0;
        d.focus_top();
        // Apps from manifests before anything lays the dock out (demo scripts aim at it).
        packages::refresh();
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
        let n = (screen_w() * screen_h()) as usize;
        let splash: Option<Buffer> = Buffer::new(screen_w() as usize, screen_h() as usize, 0);
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
        let r = r.clipped(screen_w() as usize, screen_h() as usize);
        if !r.is_empty() {
            self.damage.add(r);
        }
    }

    fn damage_all(&mut self) {
        self.damage(Rect::new(0, 0, screen_w(), screen_h()));
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
        // Already open, maybe as a tab of some window: show that tab.
        if let Some((id, t)) = self.find_app(app) {
            if let Some(w) = self.win_mut(id) {
                w.switch_to(t);
            }
            self.show_window(id);
            self.damage_window(id);
            return;
        }
        let (rect, contents): (Rect, Box<dyn WindowContents>) = apps::create(app);
        // The "+" of a strip: it opens as a tab there.
        if let Some(into) = self.add_tab_to.take()
            && let Some(w) = self.win_mut(into)
        {
            w.add_tab(app, contents);
            self.show_window(into);
            self.set_focus(Some(into));
            self.damage_window(into);
            self.invalidate(PanelKey::Dock);
            self.invalidate(PanelKey::MenuBar);
            return;
        }
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
        self.launch_with(path, None, None);
    }

    /// Start a program (or show its window if it runs), optionally sandboxed from its first
    /// instruction.
    /// Returns the new process's pid (None if it was already running, or didn't start).
    fn launch_with(
        &mut self,
        path: &str,
        caps: Option<(alloc::sync::Arc<crate::proc::caps::Caps>, String)>,
        arg: Option<&str>,
    ) -> Option<u32> {
        let name = path.rsplit('/').next().unwrap_or(path);
        if let Some(id) = self.program_window(name) {
            self.show_window(id);
            return None;
        }
        let spec = caps.as_ref().map(|c| c.1.clone());
        let argv: Vec<&str> = core::iter::once(name).chain(arg).collect();
        let started = match caps {
            Some((c, _)) => crate::proc::spawn_with_caps(path, &argv, c),
            None => crate::proc::spawn(path, &argv),
        };
        match started {
            Ok(pid) => {
                if let Some(spec) = spec {
                    crate::audit::record(
                        "sandbox.enter",
                        &alloc::format!("pid={pid} exe={path} spec={spec}"),
                    );
                }
                kprintln!("wm: started {path} as pid {pid}");
                let name = alloc::string::String::from(name);
                drop(sched::spawn("reap-app", move || {
                    let st = crate::proc::wait_kernel_child(pid);
                    // Killed by a signal (a crash): say so, with the error sound.
                    if st & 0x7f != 0 {
                        if prefs::get().sound {
                            crate::audio::play_system(crate::audio::Sound::Error);
                        }
                        notify_as(&name, &name, "quit unexpectedly");
                    }
                }));
                Some(pid)
            }
            Err(e) => {
                self.post_toast("Can't start", &alloc::format!("{path}: errno {}", e.0));
                None
            }
        }
    }

    /// A tab in the open Terminal running `cmd`, or the Terminal itself (its first tab).
    fn open_tab(&mut self, cmd: Option<String>) {
        if self.has_app(App::Terminal) {
            apps::terminal::queue_tab(cmd);
        } else if let Some(c) = cmd {
            apps::terminal::queue_command(&c);
        }
        self.open(App::Terminal);
    }

    /// An installed package: its window app, or its program in a Terminal tab.
    fn launch_package(&mut self, p: &packages::Pkg) {
        self.launch_package_with(p, None);
    }

    /// The same, with a file dropped on its dock tile: a running window app gets it as a
    /// drop on its window; a new one starts with the file granted and as its argument.
    fn launch_package_with(&mut self, p: &packages::Pkg, file: Option<&str>) {
        // System apps (the Store) are part of the OS: not sandboxed.
        if p.system && p.window {
            self.launch_with(&p.exec, None, file);
            return;
        }
        if p.window
            && let Some(f) = file
            && let Some(id) = self.program_window(p.exec.rsplit('/').next().unwrap_or(""))
        {
            self.show_window(id);
            if let Some(App::Client(cid)) = self.win(id).map(|w| w.app)
                && let Err(e) = client::drop_file(cid, 0, 0, f)
            {
                self.post_toast(&p.label, &e);
            }
            return;
        }
        if p.window {
            // Packaged window apps run sandboxed (docs/security.md): windows yes; the file
            // system is their own /Applications/<name> read-only and ~/Library/<name>
            // read-write; no network, no spawning, no clipboard; more only through the
            // powerbox, one user-chosen file at a time.
            let app_dir = alloc::format!("{}/{}", packages::DIR, p.dir);
            let lib = alloc::format!("/home/milk/Library/{}", p.dir);
            let _ = crate::fs::mkdir_all(&lib, 0o755);
            let mut grants = alloc::vec![
                crate::proc::caps::FsGrant {
                    prefix: app_dir.clone(),
                    write: false,
                },
                crate::proc::caps::FsGrant {
                    prefix: lib.clone(),
                    write: true,
                },
            ];
            // A file dropped on the tile: the user's grant, read-only, just that file.
            if let Some(f) = file {
                grants.push(crate::proc::caps::FsGrant {
                    prefix: String::from(f),
                    write: false,
                });
            }
            let caps = crate::proc::caps::Caps {
                fs: Some(grants),
                net: crate::proc::caps::Net::None,
                audio: true,
                clipboard: false,
                windows: true,
                spawn: false,
            };
            let spec = alloc::format!("fs={app_dir}:r,fs={lib}:rw,net=none,clipboard=0,spawn=0");
            if let Some(pid) =
                self.launch_with(&p.exec, Some((alloc::sync::Arc::new(caps), spec)), file)
                && let Some(f) = file
            {
                crate::audit::record(
                    "powerbox.grant",
                    &alloc::format!("pid={pid} path={f} mode=r via=drop"),
                );
            }
        } else {
            // By its /bin name when milk linked it there (a shorter tab label).
            let base = p.exec.rsplit('/').next().unwrap_or(&p.exec);
            let bin = alloc::format!("/bin/{base}");
            let cmd = if crate::fs::stat(&bin).is_ok() {
                String::from(base)
            } else {
                p.exec.clone()
            };
            self.open_tab(Some(cmd));
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
            "browser" | "web" => return self.open(App::Browser),
            "music" => return self.open(App::Music),
            _ if what.starts_with("http://") || what.starts_with("localhost") => {
                apps::browser::open_url(what);
                return self.open(App::Browser);
            }
            _ => {}
        }
        let path = apps::fsx::resolve("/", what);
        if apps::fsx::is_dir(&path) {
            apps::files::show_path(&path);
            self.open(App::Files);
        } else if path.to_ascii_lowercase().ends_with(".wav") && crate::fs::stat(&path).is_ok() {
            apps::music::play_path(&path);
            self.open(App::Music);
        } else if path.starts_with("/bin/") && crate::fs::stat(&path).is_ok() {
            self.launch(&path);
        } else if crate::fs::stat(&path).is_ok() && apps::fsx::is_text(&path) {
            apps::notes::open_file(&path);
            let sp = self.space;
            if let Some((id, _)) = self.find_app(App::Notes)
                && let Some(w) = self.win_mut(id)
            {
                w.space = sp;
            }
            self.open(App::Notes);
        } else {
            self.post_toast("Can't open", what);
        }
    }

    /// Apply new preferences (theme, wallpaper, brightness, dock size) and save them.
    fn apply_prefs(&mut self, p: prefs::Prefs) {
        let old = prefs::get();
        prefs::set(p);
        prefs::save(&p);
        if p.dark != old.dark {
            theme::set_dark(p.dark);
        }
        if p.dark != old.dark || p.wallpaper_kind() != old.wallpaper_kind() {
            match Wallpaper::new(p.wallpaper_kind()) {
                Some(wp) => self.wp = wp,
                None => self.post_toast("Out of memory", "for the new wallpaper"),
            }
        }
        if p.dock != old.dock {
            theme::set_dock_icon(p.dock);
        }
        if p.resolution != (theme::screen_w(), theme::screen_h())
            && !self.set_resolution(p.resolution.0, p.resolution.1)
        {
            self.post_toast(
                "Can't switch the display",
                "not enough memory for that size",
            );
        }
        self.settings.brightness = p.brightness;
        // Everything may look different: redraw every window and panel.
        for w in &mut self.windows {
            w.dirty = true;
        }
        self.cache.clear();
        if self.mode == Mode::Spaces {
            self.build_thumbs();
        }
        self.damage_all();
    }

    /// The /dev/wm status text.
    fn status_report(&self) -> String {
        let mode = match self.mode {
            Mode::Desktop => "desktop",
            Mode::Home => "today",
            Mode::Spaces => "spaces",
        };
        let mut s = alloc::format!(
            "mode {mode}\nlocked {}\nspace {} of {}\nfocus {}\n",
            if self.locked() { "yes" } else { "no" },
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
                (0..w.tab_count()).any(|i| {
                    matches!(w.tab_app(i), Some(App::Client(c))
                        if client::process_name(c).as_deref() == Some(name))
                })
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
        // Every tab's client is told (closing a window closes all its tabs).
        if let Some(w) = self.win(id) {
            for i in 0..w.tab_count() {
                if let Some(App::Client(cid)) = w.tab_app(i) {
                    client::closed(cid);
                }
            }
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
        let dock_top = screen_h() - DOCK_BOTTOM - dock_h();
        Rect::new(0, MENU_H, screen_w(), dock_top - MENU_H)
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
        let x = x.clamp(80 - ww, screen_w() - 80);
        let y = y.clamp(MENU_H, screen_h() - TITLE_H - 8);
        let w = self.win_mut(id).unwrap();
        if (w.rect.x, w.rect.y) == (x, y) {
            return;
        }
        w.rect.x = x;
        w.rect.y = y;
        let new = w.bounds();
        self.damage(old);
        self.damage(new);
    }

    fn resize_window(&mut self, id: u32, nw: i32, nh: i32) {
        let Some(w) = self.win(id) else {
            return;
        };
        let r = w.rect;
        let nw = nw.clamp(WIN_MIN_W, screen_w() - r.x.max(0));
        let nh = nh.clamp(WIN_MIN_H, screen_h() - r.y);
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
        self.post_toast_with("milkOS", title, sub, true);
    }

    fn post_toast_with(&mut self, app: &str, title: &str, sub: &str, sound: bool) {
        let notice = self.record_notice(app, title, sub);
        // Open drawer: the card appears at the top of its group instead of a toast.
        if self.notif {
            return;
        }
        if self.settings.focus_mode {
            kprintln!("wm: toast held back (focus mode): {title} / {sub}");
            return;
        }
        if sound && prefs::get().sound {
            crate::audio::play_system(crate::audio::Sound::Notify);
        }
        let id = self.next_toast;
        self.next_toast += 1;
        self.toasts.push(shell::Toast {
            id,
            notice,
            app: String::from(app),
            title: String::from(title),
            sub: String::from(sub),
            until_ns: timer::now_ns() + notif::TOAST_NS,
            paused: None,
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
        let mut live: Vec<PanelKey> = Vec::new();
        for l in layers {
            let Layer::Panel(spec) = l else {
                continue;
            };
            live.push(spec.key);
            // SPEC Glass: live blur over windows isn't supported, so a glass panel is always
            // composited over Wb (the blurred wallpaper) and drawn opaque: a window under the
            // notification centre, control centre, a menu, a toast or Skim never shows
            // through. (`solid`, plain milk blended over the windows, let their text read
            // through at 8%.)
            let solid = false;
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
        if !self.hold_flush && !FROZEN.load(core::sync::atomic::Ordering::Relaxed) {
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
        if self.lock_a == 255 {
            // Locked: nothing under the lock screen shows.
            let mut p = Painter::new(&mut self.back);
            let mut p = p.clipped(d);
            lock::draw(&self.lock, &mut p, d, 255);
            self.finish_rect(d, cur);
            return;
        }
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
            // A window dragged onto another's strip: 92% with a 2px straw edge (SPEC Tabs).
            let alpha = if w.ghost { 235 } else { 255 };
            p.blit_rrect(
                w.rect.x, w.rect.y, &w.surf, 0, 0, w.rect.w, w.rect.h, R_WINDOW, alpha,
            );
            if w.ghost {
                let r = w.rect;
                p.rrect_stroke(r.x, r.y, r.w, r.h, R_WINDOW, 2, STRAW, 255);
            }
        }
        if !self.anims.is_empty() {
            anim::draw(&self.anims, &self.windows, &mut p, d);
        }
        // Where a dragged window will snap.
        if let Some(r) = self.snap_preview
            && r.intersects(&d)
        {
            p.rrect_r(r, R_WINDOW, MILK, a(350));
            p.rrect_stroke(r.x, r.y, r.w, r.h, R_WINDOW, 3, HONEY, a(850));
        }
        for l in layers {
            match l {
                Layer::Dim(a) => p.fill_a(d.x, d.y, d.w, d.h, INK, *a),
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
                    let r = spec.rect;
                    p.blit_rrect(r.x, r.y, &c.buf, 0, 0, r.w, r.h, spec.radius, 255);
                }
            }
        }
        if let Some(g) = &self.dnd {
            dnd::draw(g, &mut p, d);
        }
        if let Some(s) = &self.shot
            && !self.shot_hide
        {
            shot::draw(s, &mut p, d);
        }
        if self.lock_a > 0 {
            lock::draw(&self.lock, &mut p, d, self.lock_a);
        }
        self.finish_rect(d, cur);
    }

    /// Brightness and the cursor, over everything.
    fn finish_rect(&mut self, d: Rect, cur: Rect) {
        let mut p = Painter::new(&mut self.back);
        let mut p = p.clipped(d);
        if self.settings.brightness < 100 {
            let a = ((100 - self.settings.brightness) * 255 / 100 * 7 / 10) as u8;
            p.fill_a(d.x, d.y, d.w, d.h, 0, a);
        }
        if cur.intersects(&d) && !self.shot_hide {
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
        let posted: Vec<(String, String, String, bool)> = core::mem::take(&mut *NOTIFY.lock());
        for (app, t, s, sound) in posted {
            self.post_toast_with(&app, &t, &s, sound);
        }
        let reqs: Vec<Request> = core::mem::take(&mut *REQUESTS.lock());
        for r in reqs {
            match r {
                Request::Open(a) => self.open(a),
                Request::OpenHere(a) => {
                    let sp = self.space;
                    if let Some((id, _)) = self.find_app(a)
                        && let Some(w) = self.win_mut(id)
                    {
                        w.space = sp;
                    }
                    self.open(a);
                }
                Request::Mode(m) => self.toggle_mode(m),
                Request::ClientCreate(id) => self.open(App::Client(id)),
                Request::OpenAny(what) => self.open_any(&what),
                Request::Prefs(p) => self.apply_prefs(p),
                Request::Wake => {}
                Request::Lock => self.lock_screen(),
                Request::OpenPackage(name) => {
                    packages::refresh();
                    match packages::all().into_iter().find(|p| p.name == name) {
                        Some(p) => self.launch_package(&p),
                        None => self.post_toast(&name, "isn't installed (milk install it first)"),
                    }
                }
                Request::Powerbox => {
                    if !self.has_app(App::Powerbox) && powerbox::pending() {
                        self.open(App::Powerbox);
                    }
                }
                Request::NewTab(cmd) => self.open_tab(cmd),
                Request::Skim => {
                    if self.launcher.is_none() {
                        self.toggle_launcher();
                    }
                }
                Request::ClientGone(id) => {
                    if let Some((wid, t)) = self.find_app(App::Client(id)) {
                        self.close_tab(wid, t);
                    }
                }
            }
        }
        self.tick(now);
        let animating = self.step_anims(now);
        let fading = self.lock_tick(now);
        self.sync_bars();
        self.lock_a = self.lock_alpha(timer::now_ns());
        self.render_and_compose();
        *STATS.lock() = self.stats;
        self.stats.frames != frames
            || animating
            || fading
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
        self.toasts
            .retain(|t| t.paused.is_some() || t.until_ns > now);
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
        // Dialogs that are done (the powerbox panel once answered or withdrawn).
        let done: Vec<u32> = self
            .windows
            .iter()
            .filter(|w| w.contents.wants_close())
            .map(|w| w.id)
            .collect();
        for id in done {
            self.close(id);
        }
        // Once a second: CPU, clock, live panels.
        let sec = now / 1_000_000_000;
        if sec != self.last_second {
            self.last_second = sec;
            ctl::set_status(self.status_report());
            // Installed packages (dock tiles), every few seconds.
            if sec % 3 == 1 && packages::refresh() {
                self.invalidate(PanelKey::Dock);
            }
            // Windows of userland clients that exited without destroying them.
            for cid in client::reap() {
                if let Some((wid, t)) = self.find_app(App::Client(cid)) {
                    self.close_tab(wid, t);
                }
            }
            // Per-CPU load (percpu, sampled on each CPU's tick) for the menu bar meter.
            let loads: Vec<u32> = crate::percpu::online().map(crate::percpu::load).collect();
            self.stats.cpu_pct = loads.iter().sum::<u32>() / loads.len().max(1) as u32;
            // Redraw the meter only when a bar visibly changes (8 steps).
            let steps: Vec<u32> = loads.iter().map(|l| l * 8 / 100).collect();
            let changed = steps != self.cpu_steps;
            self.cpu_loads = loads;
            self.cpu_steps = steps;
            let st = self.status_text();
            if st != self.status {
                // A new minute: the centre's clock and "n min ago".
                self.notif_tick();
            }
            if st != self.status || changed {
                self.status = st;
                self.invalidate(PanelKey::MenuBar);
            }
            if self.mode == Mode::Home {
                for i in 0..shell::card_count() {
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
        let press = matches!(
            ev,
            Event::Button { down: true, .. } | Event::Key { down: true, .. }
        );
        // Any activity (pointer and wheel too) restarts the idle-lock timer.
        if press || matches!(ev, Event::PointerMove { .. } | Event::Scroll { .. }) {
            self.last_input = timer::now_ns();
            self.idle_skew = 0;
        }
        // Choosing a screenshot region: drag it out, Esc cancels; nothing else gets through.
        if self.shot.is_some() {
            match ev {
                Event::PointerMove { x, y } => {
                    let (x, y) = (x.clamp(0, screen_w() - 1), y.clamp(0, screen_h() - 1));
                    self.damage(self.cursor_rect());
                    self.pointer = (x, y);
                    self.damage(self.cursor_rect());
                    self.shot_move(x, y);
                }
                Event::Button {
                    button: Button::Left,
                    down,
                } => {
                    if down {
                        self.shot_press();
                    } else {
                        self.shot_release();
                    }
                }
                Event::Key {
                    key: Key::Escape,
                    down: true,
                    ..
                } => self.shot_cancel(),
                _ => {}
            }
            return;
        }
        // Locked: the pointer moves, a click or a key unlocks, nothing else gets through.
        if self.locked() {
            match ev {
                Event::PointerMove { x, y } => {
                    let (x, y) = (x.clamp(0, screen_w() - 1), y.clamp(0, screen_h() - 1));
                    self.damage(self.cursor_rect());
                    self.pointer = (x, y);
                    self.damage(self.cursor_rect());
                }
                // Not in the first moments: the locking key's auto-repeat would undo it.
                _ if press && !self.lock_just_shown() => self.unlock_screen(),
                _ => {}
            }
            return;
        }
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
            Event::Button {
                button: Button::Right,
                down: true,
            } => self.right_click(),
            Event::Button { .. } => {}
            Event::Scroll { dy } => {
                let (x, y) = self.pointer;
                if self.notif && hit(&notif::rect(), x, y) {
                    self.scroll_notif(dy);
                } else if let Some(id) = self.window_at(x, y)
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
        let (x, y) = (x.clamp(0, screen_w() - 1), y.clamp(0, screen_h() - 1));
        if (x, y) == self.pointer {
            return;
        }
        self.damage(self.cursor_rect());
        self.pointer = (x, y);
        self.damage(self.cursor_rect());
        match self.drag {
            Some(Drag::Move { id, dx, dy, fresh }) => {
                let (mut dx, mut dy) = (dx, dy);
                if fresh {
                    if let Some((ndx, ndy)) = self.unsnap_for_drag(id, x, dx) {
                        (dx, dy) = (ndx, ndy);
                    }
                    self.drag = Some(Drag::Move {
                        id,
                        dx,
                        dy,
                        fresh: false,
                    });
                }
                self.move_window(id, x - dx, y - dy);
                match self.update_drop_target(id, x, y) {
                    // Over another window's strip: the ghost hangs just below that strip so
                    // the drop slot stays in sight (familiar-plus.html), no snap preview.
                    Some(below) => {
                        self.move_window(id, x - dx, below);
                        self.set_snap_preview(None);
                    }
                    None => self.set_snap_preview(snap::zone_at(x, y)),
                }
            }
            Some(Drag::Tab { id, i, x0, y0 }) => {
                // Out of the strip (down, up, or far sideways): its own window again.
                if (y - y0).abs() > 24 || (x - x0).abs() > 200 {
                    self.tear_tab(id, i, x, y);
                }
            }
            Some(Drag::Resize { id, ox, oy, w0, h0 }) => {
                self.resize_window(id, w0 + x - ox, h0 + y - oy)
            }
            Some(Drag::Content { id, x0, y0, tried }) => {
                if !tried && (x - x0).pow(2) + (y - y0).pow(2) > 36 {
                    self.drag = Some(Drag::Content {
                        id,
                        x0,
                        y0,
                        tried: true,
                    });
                    let path = self.win_mut(id).and_then(|w| {
                        let b = w.body();
                        w.contents
                            .drag_start(x0 - w.rect.x - b.x, y0 - w.rect.y - b.y)
                    });
                    if let Some(path) = path {
                        self.drag = Some(Drag::File);
                        self.dnd_start(id, path);
                        return;
                    }
                }
                self.send_pointer(id, x, y, PointerKind::Drag)
            }
            Some(Drag::File) => self.dnd_move(x, y),
            Some(Drag::Slider(i)) => self.slider_to(i, x),
            Some(_) => {}
            None => self.hover(x, y),
        }
    }

    /// Hover feedback: traffic-light symbols and the dock's name tips.
    fn hover(&mut self, x: i32, y: i32) {
        self.hover_tabs(x, y);
        // The drawer: the dismiss x follows the pointer.
        if self.notif {
            let r = notif::rect();
            self.hover_notif(hit(&r, x, y).then_some((x - r.x, y - r.y)));
        }
        // Toasts pause while the pointer is on them (SPEC: 5 s, paused on hover).
        let now = timer::now_ns();
        let over = (0..self.toasts.len()).find(|&i| hit(&shell::toast_rect(i), x, y));
        for (i, t) in self.toasts.iter_mut().enumerate() {
            match (Some(i) == over, t.paused) {
                (true, None) => t.paused = Some(t.until_ns.saturating_sub(now)),
                (false, Some(left)) => {
                    t.until_ns = now + left;
                    t.paused = None;
                }
                _ => {}
            }
        }
        // Skim: the selection follows the pointer over the results.
        if self.launcher.is_some() {
            let rows = self.layers().iter().find_map(|l| match l {
                Layer::Panel(s) if s.key == PanelKey::LaunchResults => Some(s.rect),
                _ => None,
            });
            // The rows as laid out (kind headers in between), like a click.
            if let Some(r) = rows
                && hit(&r, x, y)
                && let Some(row) = self.launcher.as_ref().and_then(|l| l.item_at(y - r.y))
                && let Some(l) = &mut self.launcher
                && row != l.sel
            {
                l.sel = row;
                self.invalidate(PanelKey::LaunchResults);
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
        // Hover states inside window bodies.
        let body = self.window_at(x, y).filter(|&id| {
            self.mode == Mode::Desktop
                && self
                    .win(id)
                    .is_some_and(|w| w.hit(x, y) == Some(Part::Body))
        });
        if body != self.hover_body
            && let Some(old) = self.hover_body
            && let Some(w) = self.win_mut(old)
            && w.contents.hover(-1, -1)
        {
            w.dirty = true;
        }
        self.hover_body = body;
        if let Some(id) = body
            && let Some(w) = self.win_mut(id)
        {
            let b = w.body();
            if w.contents.hover(x - w.rect.x - b.x, y - w.rect.y - b.y) {
                w.dirty = true;
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
        let b = w.body();
        let e = PointerInput {
            x: x - w.rect.x - b.x,
            y: y - w.rect.y - b.y,
            kind,
        };
        if w.contents.pointer(&e) {
            w.dirty = true;
        }
    }

    /// Right button: closes menus and overlays; on a window body it focuses the window and
    /// hands it the click (context menus, e.g. Files' Browse All Versions).
    fn right_click(&mut self) {
        let (x, y) = self.pointer;
        if self.menu.is_some() || self.launcher.is_some() || self.control {
            self.set_menu(None);
            self.close_launcher();
            self.control = false;
            return;
        }
        if self.mode != Mode::Desktop {
            return;
        }
        let Some(id) = self.window_at(x, y) else {
            return;
        };
        self.set_focus(Some(id));
        if self
            .win(id)
            .is_some_and(|w| w.hit(x, y) == Some(Part::Body))
        {
            self.send_pointer(id, x, y, PointerKind::Down(Button::Right));
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
        if self.notif {
            self.toggle_notif();
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
                    fresh: true,
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
            Some(p @ (Part::Tab(_) | Part::TabClose(_) | Part::TabNew)) => {
                self.tab_press(id, p, x, y);
            }
            Some(Part::Body) => {
                self.drag = Some(Drag::Content {
                    id,
                    x0: x,
                    y0: y,
                    tried: false,
                });
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
            Some(Drag::Content { id, .. }) => {
                self.send_pointer(id, x, y, PointerKind::Up(Button::Left))
            }
            Some(Drag::File) => self.dnd_drop(),
            Some(Drag::Cell { id }) => self.drop_cell(id, x, y),
            Some(Drag::Slider(0)) => {
                // Display brightness is a saved preference (Settings shows it too).
                let mut p = prefs::get();
                if p.brightness != self.settings.brightness {
                    p.brightness = self.settings.brightness;
                    prefs::set(p);
                    prefs::save(&p);
                }
            }
            Some(Drag::Move { id, .. }) if self.drop_window(id) => {
                // Merged into the window it was dropped on (tabs.rs).
                self.set_snap_preview(None);
            }
            Some(Drag::Move { id, .. }) => {
                if self.snap_preview.is_some() {
                    if let Some(z) = snap::zone_at(x, y) {
                        self.snap_to(id, z);
                    }
                    self.set_snap_preview(None);
                }
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
            Key::Char('q') if m.meta && m.ctrl => return self.lock_screen(),
            // Screenshots: Cmd+Shift+3 the screen, Cmd+Shift+4 a region.
            Key::Char('3' | '#') if m.meta && m.shift => return self.capture(None),
            Key::Char('4' | '$') if m.meta && m.shift => return self.shot_region_start(),
            Key::Escape if self.menu.is_some() => return self.set_menu(None),
            Key::Escape if self.launcher.is_some() => {
                self.add_tab_to = None;
                return self.close_launcher();
            }
            Key::Escape if self.control => {
                self.control = false;
                return;
            }
            Key::Escape if self.notif => return self.close_notif(),
            // Ctrl+N: the notification drawer.
            Key::Char('n') if m.ctrl && !m.meta && !m.alt => return self.toggle_notif(),
            Key::Escape if self.mode != Mode::Desktop => return self.set_mode(Mode::Desktop),
            Key::F(3) => return self.toggle_mode(Mode::Spaces),
            Key::Up if m.ctrl && !m.alt => return self.toggle_mode(Mode::Spaces),
            Key::F(4) => return self.toggle_mode(Mode::Home),
            // Ctrl+Alt+arrows: snap the focused window.
            Key::Left | Key::Right | Key::Up | Key::Down if m.ctrl && m.alt => {
                if let Some(id) = self.focus {
                    match k.key {
                        Key::Left => self.snap_to(id, snap::Zone::Left),
                        Key::Right => self.snap_to(id, snap::Zone::Right),
                        Key::Up => self.snap_to(id, snap::Zone::Fill),
                        _ => self.unsnap(id),
                    }
                }
                return;
            }
            Key::Left if m.ctrl => return self.switch_space(self.space.saturating_sub(1)),
            Key::Right if m.ctrl => return self.switch_space(self.space + 1),
            _ => {}
        }
        if self.notif && self.notif_key(&k) {
            return;
        }
        if self.launcher.is_some() {
            return self.launcher_key(&k);
        }
        if m.meta {
            let mut handled = true;
            match k.key {
                Key::Char('w') => {
                    if let Some(id) = self.focus {
                        if let Some(w) = self.win_mut(id)
                            && w.contents.close_part()
                        {
                            w.dirty = true;
                        } else if let Some(w) = self.win(id)
                            && w.tab_count() > 1
                        {
                            // The window tab, then (when it is the last) the window.
                            let a = w.active;
                            self.close_tab(id, a);
                        } else {
                            self.close(id);
                        }
                    }
                }
                Key::Char('m') => {
                    if let Some(id) = self.focus {
                        self.minimise(id);
                    }
                }
                Key::Tab | Key::Char('`') => self.cycle_windows(),
                // Cmd+Shift+[ / ]: the window's previous / next tab.
                Key::Char('[' | '{') if m.shift => self.cycle_tab(-1),
                Key::Char(']' | '}') if m.shift => self.cycle_tab(1),
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
