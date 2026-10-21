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
            b.typ("help")
                .enter()
                .wait(100)
                .typ("cowsay milkOS 1.0")
                .enter()
                .typ("uname -a");
        }
        "notes" => {
            let (x, y) = dock_icon_center(3);
            b.click(x, y).wait(200);
            b.key(Key::End, Mods::NONE)
                .enter()
                .typ("write the window manager");
        }
        "about" => {
            let (x, y) = dock_icon_center(7);
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
        "toast" => {
            b.0.push(Step::Toast("Dentist", "in 15 minutes"));
            b.wait(300);
            b.0.push(Step::Toast("Build finished", "milkOS in 2.1 s"));
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
    let mut buf = [0u8; 32];
    let n = (f.size as usize).min(buf.len());
    if !fwcfg::read_file(f, &mut buf[..n]) {
        return None;
    }
    let name = core::str::from_utf8(&buf[..n])
        .ok()?
        .trim_matches(char::from(0))
        .trim();
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
    /// Want the main loop to skip `wfi`? (Only between closely spaced steps.)
    pub fn busy(&self) -> bool {
        false
    }

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
            }
        }
        false
    }
}
