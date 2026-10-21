//! Canned input for headless demos and benchmarks. Pick one at boot with
//! `-fw_cfg name=opt/milkos/demo,string=<name>` (`scripts/screenshot.sh out.png 6 -fw_cfg ...`).
//! Scripts go through exactly the same event path as real input. Each logs `[demo] <name> done`
//! when finished, so a test can wait for that line before taking its screenshot.

use alloc::string::String;
use alloc::vec::Vec;

use crate::{fwcfg, kprintln};

use super::Desktop;
use super::event::{Button, Event, Key, Mods};
use super::shell::dock_icon_center;

pub enum Step {
    Wait(u64),
    Ev(Event),
    Toast(&'static str, &'static str),
    Bench,
    Log(&'static str),
    Panic,
}

pub struct Script {
    name: String,
    steps: Vec<Step>,
    i: usize,
    wait_until: u64,
}

struct B(Vec<Step>);

impl B {
    fn wait(&mut self, ms: u64) -> &mut Self {
        self.0.push(Step::Wait(ms));
        self
    }
    fn ev(&mut self, e: Event) -> &mut Self {
        self.0.push(Step::Ev(e));
        self
    }
    fn to(&mut self, x: i32, y: i32) -> &mut Self {
        self.ev(Event::PointerMove { x, y })
    }
    fn down(&mut self) -> &mut Self {
        self.ev(Event::Button {
            button: Button::Left,
            down: true,
        })
    }
    fn up(&mut self) -> &mut Self {
        self.ev(Event::Button {
            button: Button::Left,
            down: false,
        })
    }
    fn click(&mut self, x: i32, y: i32) -> &mut Self {
        self.to(x, y).wait(30).down().wait(30).up().wait(60)
    }
    /// Press at (x0, y0), glide to (x1, y1) in `n` steps of ~16 ms, release.
    fn drag(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, n: i32) -> &mut Self {
        self.to(x0, y0).wait(30).down().wait(30);
        for i in 1..=n {
            self.to(x0 + (x1 - x0) * i / n, y0 + (y1 - y0) * i / n)
                .wait(16);
        }
        self.up().wait(60)
    }
    fn glide(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, n: i32) -> &mut Self {
        for i in 1..=n {
            self.to(x0 + (x1 - x0) * i / n, y0 + (y1 - y0) * i / n)
                .wait(16);
        }
        self
    }
    fn key(&mut self, k: Key, m: Mods) -> &mut Self {
        self.ev(Event::key(k, m)).wait(40)
    }
    fn typ(&mut self, s: &str) -> &mut Self {
        for c in s.chars() {
            self.ev(Event::char(c)).wait(25);
        }
        self
    }
    fn enter(&mut self) -> &mut Self {
        self.key(Key::Enter, Mods::NONE)
    }
}

// The starting desktop (see apps::create): Files (96,64) 600x400, Terminal (440,270) 520x290.
const TERM_MIN: (i32, i32) = (440 + 14 + 20 + 6, 270 + 16);
const TERM_ZOOM: (i32, i32) = (440 + 14 + 40 + 6, 270 + 16);
const FILES_CLOSE: (i32, i32) = (96 + 14 + 6, 64 + 16);

fn build(name: &str) -> Option<Vec<Step>> {
    let mut b = B(Vec::new());
    b.wait(400);
    match name {
        "drag" => {
            b.drag(300, 80, 420, 150, 40);
        }
        "focus" => {
            b.click(200, 300);
        }
        "minimise" => {
            b.click(TERM_MIN.0, TERM_MIN.1);
        }
        "restore" => {
            b.click(TERM_MIN.0, TERM_MIN.1).wait(400);
            let (x, y) = dock_icon_center(1);
            b.click(x, y);
        }
        "zoom" => {
            b.click(TERM_ZOOM.0, TERM_ZOOM.1);
        }
        "resize" => {
            b.drag(956, 556, 1010, 660, 20);
        }
        "close" => {
            b.click(FILES_CLOSE.0, FILES_CLOSE.1);
        }
        "type" => {
            b.wait(300).typ("help").enter().wait(300);
            b.typ("ps").enter().wait(300).typ("uname -a");
        }
        "notes" => {
            let (x, y) = dock_icon_center(3);
            b.click(x, y).wait(200);
            b.key(Key::End, Mods::NONE)
                .enter()
                .typ("write the window manager");
        }
        "about" => {
            let (x, y) = dock_icon_center(8);
            b.click(x, y).wait(1200);
        }
        "launcher" => {
            b.key(Key::Char(' '), Mods::META).wait(100).typ("te");
        }
        "launch" => {
            b.key(Key::Char(' '), Mods::META)
                .wait(100)
                .typ("about")
                .wait(200)
                .enter();
        }
        "today" => {
            b.key(Key::F(4), Mods::NONE);
        }
        "spaces" => {
            b.key(Key::F(3), Mods::NONE);
        }
        "space2" => {
            b.key(Key::Right, Mods::CTRL);
        }
        "control" => {
            b.click(1000, 14).wait(100);
        }
        "dim" => {
            b.click(1000, 14).wait(100);
            b.drag(824, 160, 760, 160, 10);
        }
        "shell" => {
            b.wait(300)
                .typ("pwd")
                .enter()
                .wait(200)
                .typ("ls")
                .enter()
                .wait(200);
            b.typ("cat /etc/motd").enter().wait(300);
        }
        "files" => {
            // Files: sidebar Home, double-click Documents, double-click ideas.md (opens Notes).
            b.click(137, 153).wait(200);
            b.click(421, 145).wait(40).click(421, 145).wait(300);
            b.click(321, 145).wait(40).click(321, 145).wait(300);
        }
        "browse" => {
            b.click(137, 153).wait(200);
        }
        "stty" => {
            b.wait(300)
                .typ("stty")
                .enter()
                .wait(200)
                .typ("stty size")
                .enter()
                .wait(200);
            b.typ("stty -echo")
                .enter()
                .wait(200)
                .typ("echo hidden")
                .enter()
                .wait(200);
            b.typ("stty sane")
                .enter()
                .wait(200)
                .typ("stty")
                .enter()
                .wait(300);
        }
        "paint" => {
            // Dock icon 4 is Paint. Its window lands at (192, 112); the canvas starts 48px
            // into the body, so draw a smiling cow-ish face in screen coordinates.
            let (x, y) = dock_icon_center(4);
            b.click(x, y).wait(1200);
            b.click(193 + 12 + 34 + 14, 145 + 24).wait(100); // straw swatch
            b.drag(320, 280, 520, 280, 24).wait(50);
            b.drag(520, 280, 520, 420, 16).wait(50);
            b.drag(520, 420, 320, 420, 24).wait(50);
            b.drag(320, 420, 320, 280, 16).wait(50);
            b.click(193 + 12 + 4 * 34 + 14, 145 + 24).wait(100); // blue
            b.drag(380, 340, 381, 341, 2).drag(460, 340, 461, 341, 2);
            b.click(193 + 12 + 2 * 34 + 14, 145 + 24).wait(100); // honey
            b.drag(370, 385, 420, 400, 10)
                .drag(420, 400, 470, 385, 10)
                .wait(300);
        }
        "paintclose" => {
            let (x, y) = dock_icon_center(4);
            b.click(x, y)
                .wait(1200)
                .click(192 + 14 + 6, 112 + 16)
                .wait(800);
            b.typ("ps").enter().wait(400);
        }
        "desk" => {
            b.wait(300)
                .typ("cowsay hello from userland")
                .enter()
                .wait(300);
            b.typ("notify Tea is ready in 3 minutes").enter().wait(300);
            b.typ("cat /dev/wm").enter().wait(300);
            b.typ("open ~/Documents/todo.txt").enter().wait(600);
        }
        "desktoday" => {
            b.wait(300).typ("today").enter().wait(600);
        }
        "minianim" => {
            // Ends right after the release, so a quick screenshot catches the animation.
            b.to(TERM_MIN.0, TERM_MIN.1).wait(30).down().wait(30).up();
        }
        "minianim2" => {
            b.to(TERM_MIN.0, TERM_MIN.1)
                .wait(30)
                .down()
                .wait(30)
                .up()
                .wait(4000);
            b.key(Key::F(4), Mods::NONE)
                .wait(300)
                .key(Key::F(4), Mods::NONE)
                .wait(300);
        }
        "dockpress" => {
            // Hover the Terminal tile, then press (and hold) the Notes tile and a light.
            let (x, y) = dock_icon_center(1);
            b.glide(600, 500, x, y, 8).wait(200);
            let (x, y) = dock_icon_center(3);
            b.glide(x - 60, y, x, y, 4).down().wait(300);
        }
        "select" => {
            // Notes: open it, drag-select two lines, copy, paste at the end.
            let (x, y) = dock_icon_center(3);
            b.click(x, y).wait(300);
            // Notes body: window (120, 96), body at (121, 129), text at +16 / 20px lines.
            b.drag(
                121 + 16 + 16,
                129 + 16 + 20 + 8,
                121 + 16 + 8 * 18,
                129 + 16 + 40 + 8,
                10,
            );
            b.key(Key::Char('c'), Mods::META).key(Key::End, Mods::CTRL);
            b.key(Key::Down, Mods::NONE)
                .key(Key::Down, Mods::NONE)
                .key(Key::Down, Mods::NONE);
            b.key(Key::End, Mods::NONE).key(Key::Enter, Mods::NONE);
            b.key(Key::Char('v'), Mods::META).wait(100);
            b.drag(
                121 + 16 + 16,
                129 + 16 + 8,
                121 + 16 + 8 * 11,
                129 + 16 + 8,
                6,
            )
            .wait(200);
        }
        "termselect" => {
            // Terminal: select a few words of output with the mouse, copy, paste at the prompt.
            b.wait(300).typ("echo milk and honey").enter().wait(300);
            b.drag(455, 507 - 18 + 8, 455 + 8 * 14, 507 - 18 + 8, 8);
            b.key(Key::Char('c'), Mods::META)
                .key(Key::Char('v'), Mods::META)
                .wait(200);
            b.drag(455, 507 - 36 + 8, 455 + 8 * 10, 507 - 36 + 8, 6)
                .wait(200);
        }
        "openanim" => {
            let (x, y) = dock_icon_center(8);
            b.to(x, y).wait(30).down().wait(30).up();
        }
        "lsbin" => {
            b.wait(300).typ("ls /bin").enter().wait(600);
        }
        "sigint" => {
            b.wait(300).typ("sleep 9").enter().wait(1000);
            b.key(Key::Char('c'), Mods::CTRL)
                .wait(400)
                .typ("echo status $?")
                .enter()
                .wait(400);
        }
        "winsize" => {
            // Shrink the Terminal from its corner, then ask the tty its size.
            b.wait(300).typ("stty size").enter().wait(300);
            b.drag(956, 556, 800, 500, 12).wait(200);
            b.typ("stty size").enter().wait(400);
        }
        "utest" => {
            // proc's userland test suite (fork / CoW, pipes, signals) from the desktop shell.
            b.wait(300).typ("utest").enter().wait(6000);
        }
        "hover" => {
            b.glide(600, 400, 480, 286, 10);
        }
        "menu" => {
            b.click(320, 14).glide(320, 14, 330, 70, 8);
        }
        "movespace" => {
            b.key(Key::F(3), Mods::NONE).wait(300);
            b.drag(540, 275, 582, 84, 20).wait(300).click(582, 84);
        }
        "toast" => {
            b.0.push(Step::Toast("Dentist", "in 15 minutes"));
            b.wait(300);
            b.0.push(Step::Toast("Build finished", "milkOS in 2.1 s"));
        }
        "panic" => {
            b.0.push(Step::Panic);
        }
        "shutdown" => {
            b.click(60, 14).wait(100).click(80, 219);
        }
        "logo" => {
            b.click(60, 14).glide(60, 14, 90, 60, 6);
        }
        "bench" => {
            b.0.push(Step::Bench);
        }
        "tour" => {
            b.glide(512, 384, 300, 80, 20)
                .drag(300, 80, 360, 120, 30)
                .wait(500);
            b.glide(360, 120, 700, 420, 20).click(700, 420);
            b.typ("cowsay hello Andrew").enter().wait(1500);
            b.key(Key::Char(' '), Mods::META)
                .wait(300)
                .typ("not")
                .wait(600)
                .enter()
                .wait(1200);
            b.key(Key::F(3), Mods::NONE)
                .wait(2000)
                .key(Key::Escape, Mods::NONE)
                .wait(500);
            b.key(Key::F(4), Mods::NONE)
                .wait(2500)
                .key(Key::F(4), Mods::NONE)
                .wait(500);
            b.0.push(Step::Toast("Tour done", "it's all yours"));
        }
        _ => return None,
    }
    b.0.push(Step::Log("done"));
    Some(b.0)
}

pub fn from_fw_cfg() -> Option<Script> {
    let f = fwcfg::find_file("opt/milkos/demo")?;
    // Heap, not stack: fw_cfg reads by DMA to this address, and thread stacks live in a
    // kernel-virtual window that isn't identity mapped.
    let mut buf = alloc::vec![0u8; 32];
    let n = (f.size as usize).min(buf.len());
    if !fwcfg::read_file(f, &mut buf[..n]) {
        return None;
    }
    let name = core::str::from_utf8(&buf[..n])
        .ok()?
        .trim_matches(char::from(0))
        .trim();
    if name == "none" || name.is_empty() {
        return None;
    }
    // slow-<demo>: the same demo with animations 20x slower (for catching them on camera).
    let name = match name.strip_prefix("slow-") {
        Some(n) => {
            super::anim::SLOWMO.store(20, core::sync::atomic::Ordering::Relaxed);
            n
        }
        None => name,
    };
    let Some(steps) = build(name) else {
        kprintln!("[demo] unknown demo '{name}'");
        return None;
    };
    kprintln!("[demo] {name}: {} steps", steps.len());
    Some(Script {
        name: String::from(name),
        steps,
        i: 0,
        wait_until: 0,
    })
}

impl Script {
    /// Run due steps; at most one input event per frame, so each one gets composited. Returns
    /// true when the script is finished.
    pub fn step(&mut self, d: &mut Desktop, now: u64) -> bool {
        if self.wait_until == 0 {
            self.wait_until = now;
        }
        while now >= self.wait_until {
            let Some(s) = self.steps.get(self.i) else {
                return true;
            };
            self.i += 1;
            match s {
                Step::Wait(ms) => self.wait_until = now + ms * 1_000_000,
                Step::Ev(e) => {
                    d.handle(*e);
                    return false;
                }
                Step::Toast(t, s) => d.post_toast(t, s),
                Step::Bench => d.bench(),
                Step::Log(m) => kprintln!("[demo] {} {}", self.name, m),
                Step::Panic => {
                    kprintln!("[demo] {} done", self.name);
                    panic!("demo: deliberate panic to show the panic screen");
                }
            }
        }
        false
    }
}
