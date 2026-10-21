//! Canned input for headless demos and benchmarks. Pick one at boot with
//! `-fw_cfg name=opt/milkos/demo,string=<name>` (`scripts/screenshot.sh out.png 6 -fw_cfg ...`).
//! Scripts go through exactly the same event path as real input. Each logs `[demo] <name> done`
//! when finished, so a test can wait for that line before taking its screenshot.

use alloc::string::String;
use alloc::vec::Vec;

use crate::{fwcfg, kprintln};

use super::Desktop;
use super::event::{Button, Event, Key, Mods};
use super::shell::{dock_icon_center, dock_icon_center_of};

pub enum Step {
    Wait(u64),
    Ev(Event),
    Toast(&'static str, &'static str),
    Bench,
    Log(&'static str),
    Panic,
    /// Pretend nobody touched anything for this many seconds (the idle lock).
    Idle(u64),
    /// Switch the display mode.
    Resolution(i32, i32),
    /// Chocolate milk (true) or Whole milk.
    Theme(bool),
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
    fn rclick(&mut self, x: i32, y: i32) -> &mut Self {
        self.to(x, y).wait(30);
        self.ev(Event::Button {
            button: Button::Right,
            down: true,
        })
        .wait(30)
        .ev(Event::Button {
            button: Button::Right,
            down: false,
        })
        .wait(60)
    }
    /// In the Terminal: a file with three snapshotted versions and a newer current one.
    fn herd(&mut self) -> &mut Self {
        const F: &str = "/home/milk/Documents/herd.txt";
        self.wait(300);
        for (i, line) in ["- one cow", "- two cows", "- three cows", "- a calf!"]
            .iter()
            .enumerate()
        {
            let op = if i == 0 { ">" } else { ">>" };
            self.typ(&alloc::format!("echo '{line}' {op} {F}"))
                .enter()
                .wait(250);
            if i < 3 {
                self.typ(&alloc::format!("snap create herd{}", i + 1))
                    .enter()
                    .wait(500);
            }
        }
        self
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
            b.key(Key::Char(' '), Mods::META)
                .wait(100)
                .typ("about")
                .wait(150)
                .enter()
                .wait(1200);
        }
        "settings" => {
            let (x, y) = dock_icon_center(8);
            b.click(x, y).wait(500);
        }
        "dark" => {
            // Settings > Appearance > Chocolate milk.
            let (x, y) = dock_icon_center(8);
            b.click(x, y)
                .wait(500)
                .click(181 + 188 + 212 + 98, 113 + 92 + 68)
                .wait(800);
        }
        "wallpapers" => {
            let (x, y) = dock_icon_center(8);
            b.click(x, y)
                .wait(500)
                .click(181 + 60, 113 + 44 + 28 + 12)
                .wait(800);
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
            b.click(915, 14).wait(100);
        }
        "dim" => {
            b.click(915, 14).wait(100);
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
            // Dock icon 4 is Paint. Its window was designed at (192, 112) (client::place
            // centres it now: (dx, dy) is the difference); the canvas starts 48px into the
            // body, so draw a smiling cow-ish face in screen coordinates.
            let (dx, dy) = paint_offset();
            let (x, y) = dock_icon_center(4);
            b.click(x, y).wait(1200);
            b.click(dx + 193 + 12 + 34 + 14, dy + 145 + 24).wait(100); // straw swatch
            b.drag(dx + 320, dy + 280, dx + 520, dy + 280, 24).wait(50);
            b.drag(dx + 520, dy + 280, dx + 520, dy + 420, 16).wait(50);
            b.drag(dx + 520, dy + 420, dx + 320, dy + 420, 24).wait(50);
            b.drag(dx + 320, dy + 420, dx + 320, dy + 280, 16).wait(50);
            b.click(dx + 193 + 12 + 4 * 34 + 14, dy + 145 + 24)
                .wait(100); // blue
            b.drag(dx + 380, dy + 340, dx + 381, dy + 341, 2).drag(
                dx + 460,
                dy + 340,
                dx + 461,
                dy + 341,
                2,
            );
            b.click(dx + 193 + 12 + 2 * 34 + 14, dy + 145 + 24)
                .wait(100); // honey
            b.drag(dx + 370, dy + 385, dx + 420, dy + 400, 10)
                .drag(dx + 420, dy + 400, dx + 470, dy + 385, 10)
                .wait(300);
        }
        "paintclose" => {
            let (dx, dy) = paint_offset();
            let (x, y) = dock_icon_center(4);
            b.click(x, y)
                .wait(1200)
                .click(dx + 192 + 14 + 6, dy + 112 + 16)
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
        // Terminal tabs (the boot Terminal has the focus).
        "tabs" => {
            b.wait(300).key(Key::Char('t'), Mods::META).wait(500);
            b.typ("echo second tab on $(tty)").enter().wait(300);
            b.typ("echo second tab").enter().wait(300);
            b.key(Key::Char('t'), Mods::META).wait(500);
            b.typ("ls /bin").enter().wait(600);
        }
        "tabswitch" => {
            b.wait(300).key(Key::Char('t'), Mods::META).wait(500);
            b.typ("echo this is tab 2").enter().wait(300);
            b.key(Key::Char('1'), Mods::META).wait(300);
            b.typ("echo back in tab 1").enter().wait(400);
        }
        "tabclose" => {
            // Three tabs, close the third (Cmd+W), then ps in the first: msh on tty1 and
            // tty2 only.
            b.wait(300).key(Key::Char('t'), Mods::META).wait(500);
            b.key(Key::Char('t'), Mods::META).wait(500);
            b.key(Key::Char('w'), Mods::META).wait(500);
            b.key(Key::Char('1'), Mods::META).wait(200);
            b.typ("ps").enter().wait(600);
        }
        "tabcmd" => {
            // Skim's "> cmd" with the Terminal open: a new tab running it.
            b.wait(300).key(Key::Char(' '), Mods::META).wait(150);
            b.typ("> uname -a").wait(200).enter().wait(1200);
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
        "tm1" => {
            // Settings > Time Machine > Back up now, close Settings, delete a file, then
            // Files > Time Machine > newest snapshot.
            let (x, y) = dock_icon_center(8);
            b.click(x, y)
                .wait(500)
                .click(241, 309)
                .wait(200)
                .click(556, 304)
                .wait(800);
            b.click(200, 96).wait(300);
            b.click(700, 450)
                .typ("rm Documents/todo.txt")
                .enter()
                .wait(400);
            b.click(200, 300).wait(200).click(137, 415).wait(300);
        }
        "tm2" => {
            let (x, y) = dock_icon_center(8);
            b.click(x, y)
                .wait(500)
                .click(241, 309)
                .wait(200)
                .click(556, 304)
                .wait(800);
            b.click(200, 96).wait(300);
            b.click(700, 450)
                .typ("rm Documents/todo.txt")
                .enter()
                .wait(400);
            b.click(200, 300).wait(200).click(137, 415).wait(300);
            b.click(297, 193).wait(300);
        }
        "timemachine" => {
            let (x, y) = dock_icon_center(8);
            b.click(x, y)
                .wait(500)
                .click(241, 309)
                .wait(200)
                .click(556, 304)
                .wait(800);
            b.click(200, 96).wait(300);
            b.click(700, 450)
                .typ("rm Documents/todo.txt")
                .enter()
                .wait(400);
            b.click(200, 300).wait(200).click(137, 415).wait(300);
            b.click(297, 193).wait(300); // newest snapshot: its home folder
            b.click(421, 145).wait(40).click(421, 145).wait(300); // Documents
            b.click(421, 145).wait(200); // select todo.txt
            b.click(593, 451).wait(300); // Restore
            b.click(137, 181).wait(400); // live Documents: it's back
        }
        // Music: window at (300, 92), body at (301, 125), 458 wide. Play at body (229, 182);
        // rows from body y 238 (MUSIC header at 214), SYSTEM SOUNDS header after them.
        "music" => {
            let (x, y) = dock_icon_center(5);
            b.click(x, y)
                .wait(800)
                .key(Key::Char(' '), Mods::NONE)
                .wait(3000);
        }
        "musicpause" => {
            let (x, y) = dock_icon_center(5);
            b.click(x, y)
                .wait(800)
                .click(301 + 229, 125 + 182)
                .wait(1500);
            b.click(301 + 229, 125 + 182).wait(800);
        }
        "musicseek" => {
            // Play, then click three quarters along the progress bar.
            let (x, y) = dock_icon_center(5);
            b.click(x, y)
                .wait(800)
                .click(301 + 229, 125 + 182)
                .wait(600);
            b.click(301 + 20 + 418 * 3 / 4, 125 + 135).wait(1200);
        }
        "musicsys" => {
            // Second track down, then the chime from System Sounds (click selects, again plays).
            let (x, y) = dock_icon_center(5);
            b.click(x, y).wait(800);
            b.key(Key::Down, Mods::NONE).enter().wait(1000);
            b.click(301 + 200, 125 + 318 + 12)
                .wait(150)
                .click(301 + 200, 125 + 318 + 12)
                .wait(600);
        }
        "browser" => {
            // Dock globe: the Browser opens milkOS's own home page (kernel httpd).
            let (x, y) = dock_icon_center(2);
            b.click(x, y).wait(2500);
        }
        "browse2" => {
            // Follow a link (the kernel page's /net), then go back.
            let (x, y) = dock_icon_center(2);
            b.click(x, y).wait(2500);
            b.key(Key::Char('l'), Mods::META).wait(100);
            for _ in 0..40 {
                b.key(Key::Backspace, Mods::NONE);
            }
            b.typ(&host_test_url()).enter().wait(1500);
        }
        "browselink" => {
            let (x, y) = dock_icon_center(2);
            b.click(x, y).wait(2500).click(220, 290).wait(2000);
        }
        "browseback" => {
            let (x, y) = dock_icon_center(2);
            b.click(x, y)
                .wait(2500)
                .click(220, 290)
                .wait(2000)
                .click(175, 111)
                .wait(2000);
        }
        "browse3" => {
            // On the python test site: follow the relative link, then the absolute one back.
            let (x, y) = dock_icon_center(2);
            b.click(x, y).wait(2500);
            b.key(Key::Char('l'), Mods::META).wait(100);
            for _ in 0..40 {
                b.key(Key::Backspace, Mods::NONE);
            }
            b.typ(&host_test_url()).enter().wait(1500);
            b.click(230, 312).wait(1500);
        }
        "browserdeny" => {
            let (x, y) = dock_icon_center(2);
            b.click(x, y).wait(1500);
            b.key(Key::Char('l'), Mods::META).wait(100);
            for _ in 0..40 {
                b.key(Key::Backspace, Mods::NONE);
            }
            b.typ("http://example.com/").enter().wait(800);
        }
        "ansi" => {
            b.wait(300);
            b.typ(r"/bin/echo -e '\e[31mred \e[32mgreen \e[33myellow \e[34mblue \e[35mmagenta \e[36mcyan\e[0m'").enter().wait(300);
            b.typ(
                r"/bin/echo -e '\e[1;31mbold red \e[1;32mbold green \e[90mgrey\e[0m back to plain'",
            )
            .enter()
            .wait(300);
        }
        "clear" => {
            b.wait(300)
                .typ("ls /bin")
                .enter()
                .wait(400)
                .typ("clear")
                .enter()
                .wait(400);
            b.typ("echo the screen was cleared").enter().wait(300);
        }
        "skimsearch" => {
            b.key(Key::Char(' '), Mods::META).wait(150).typ("note");
        }
        "skimcalc" => {
            b.key(Key::Char(' '), Mods::META)
                .wait(150)
                .typ("= (2+2*3)^2/4 + 0.5");
        }
        "skimfile" => {
            b.key(Key::Char(' '), Mods::META).wait(150).typ("todo");
        }
        "skimcmd" => {
            b.key(Key::Char(' '), Mods::META)
                .wait(150)
                .typ("> ls /bin")
                .wait(200)
                .enter()
                .wait(1200);
        }
        "skimaction" => {
            b.key(Key::Char(' '), Mods::META)
                .wait(150)
                .typ("dark")
                .wait(200)
                .enter()
                .wait(800);
        }
        "snapleft" => {
            // Drag Files to the left edge: preview, then let go (snaps to the left half).
            b.drag(300, 80, 2, 300, 24);
        }
        "snappreview" => {
            // Hold Files at the top-right corner: the quarter preview shows.
            b.to(300, 80)
                .wait(30)
                .down()
                .wait(30)
                .glide(300, 80, 1021, 60, 24)
                .wait(200);
        }
        "snapkeys" => {
            // Terminal focused: Ctrl+Alt+Right, then Files to the left by dragging.
            let ca = Mods {
                ctrl: true,
                alt: true,
                ..Mods::NONE
            };
            b.key(Key::Right, ca).wait(200);
            b.drag(300, 80, 2, 300, 20).wait(200);
        }
        "unsnap" => {
            // Snap Files left, then drag it away from the edge: it gets its old size back.
            b.drag(300, 80, 2, 300, 20).wait(200);
            b.drag(200, 50, 500, 200, 20).wait(200);
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
        // Lock screen.
        "lock" => {
            b.wait(300).key(Key::Char('q'), Mods::META_CTRL).wait(700);
        }
        "unlock" => {
            // Lock, then a key unlocks (and is not typed into the Terminal).
            b.wait(300).key(Key::Char('q'), Mods::META_CTRL).wait(700);
            b.typ("x").wait(700);
        }
        // Packages (docs/packages.md), against a repo served by scripts/wm-pkg-demo.sh: install
        // fortune and moo2048 from the Terminal; their dock tiles appear within 3 s.
        "pkgdock" | "pkgrun" | "pkgfortune" | "pkgsandbox" => {
            let url = host_test_url();
            b.wait(300)
                .typ(&alloc::format!("echo {url} > /etc/milk/repos"))
                .enter()
                .wait(300);
            b.typ("milk update").enter().wait(2000);
            b.typ("milk install fortune").enter().wait(3000);
            b.typ("milk install moo2048").enter().wait(3000);
            b.typ("clear").enter().wait(3500);
            // After the Store (tile 9), by name: fortune is tile 10, moo2048 tile 11.
            if name == "pkgrun" || name == "pkgsandbox" {
                let (x, y) = dock_icon_center_of(11, 3);
                b.click(x, y).wait(2000);
            }
            if name == "pkgsandbox" {
                // moo2048 runs sandboxed: the audit log says so; play a move to show it works.
                b.key(Key::Left, Mods::NONE).wait(300);
                let (x, y) = dock_icon_center_of(1, 3);
                b.click(x, y).wait(400);
                b.typ("audit --event sandbox").enter().wait(800);
            } else if name == "pkgfortune" {
                let (x, y) = dock_icon_center_of(10, 3);
                b.click(x, y).wait(2000);
            } else {
                let (x, y) = dock_icon_center_of(11, 3);
                b.glide(x, y - 80, x, y, 6).wait(300);
            }
        }
        // Versions: Files (window (96, 64), body (97, 97)), herd.txt is the first icon.
        "versions" | "versionsrestore" => {
            b.herd();
            let (x, y) = dock_icon_center(0);
            b.click(x, y).wait(1500);
            b.rclick(321, 150).wait(300);
            b.click(321 + 40, 150 + 6 + 26 + 10).wait(1200);
            if name == "versionsrestore" {
                // One version further back (herd2), then Restore (window (110, 64) 800 x 500).
                b.key(Key::Down, Mods::NONE).wait(300);
                b.click(111 + 798 - 16 - 90, 97 + 466 - 44 + 22).wait(1200);
            }
        }
        "filesctx" => {
            b.herd();
            let (x, y) = dock_icon_center(0);
            b.click(x, y).wait(1500);
            b.rclick(321, 150).wait(400);
        }
        "notesversions" => {
            b.herd();
            b.typ("open /home/milk/Documents/herd.txt")
                .enter()
                .wait(1000);
            b.click(115, 14).wait(400).click(160, 130).wait(1200);
        }
        // Powerbox: peek sandboxes itself (no fs), asks for a file through the wm's panel.
        "powerbox" | "powerboxpick" | "powerboxsave" | "powerboxaudit" => {
            b.wait(300).typ("open /bin/peek").enter().wait(1500);
            b.key(Key::Char('o'), Mods::NONE).wait(900);
            if name != "powerbox" {
                // Documents: ideas.md, todo.txt. Pick todo.txt.
                b.key(Key::Down, Mods::NONE)
                    .key(Key::Down, Mods::NONE)
                    .wait(200)
                    .enter()
                    .wait(1000);
            }
            if name == "powerboxsave" || name == "powerboxaudit" {
                // Save a copy: the suggested name, Enter.
                b.key(Key::Char('s'), Mods::NONE)
                    .wait(900)
                    .enter()
                    .wait(1000);
            }
            if name == "powerboxaudit" {
                let (x, y) = dock_icon_center(1);
                b.click(x, y).wait(400);
                b.typ("clear").enter().wait(200);
                b.typ("ls ~/Documents").enter().wait(300);
                b.typ("audit --event powerbox").enter().wait(500);
                b.typ("audit --event sandbox").enter().wait(800);
            }
        }
        // Screenshots (Cmd+Shift+3 / 4), saved to ~/Pictures.
        "shot" | "shotregion" | "shotselect" => {
            let cs = Mods {
                meta: true,
                shift: true,
                ..Mods::NONE
            };
            if name == "shot" {
                b.wait(300).key(Key::Char('3'), cs).wait(2500);
            } else {
                b.wait(300).key(Key::Char('4'), cs).wait(300);
                b.to(300, 90).wait(30).down().wait(30);
                for i in 1..=12 {
                    b.to(300 + 32 * i, 90 + 25 * i).wait(16);
                }
                if name == "shotregion" {
                    b.up().wait(2500);
                } else {
                    b.wait(300);
                }
            }
        }
        // Display modes: Settings > Display & Lock > Resolution (buttons at body y 219).
        "res1280" | "res1440" | "resback" | "res1440click" => {
            let (x, y) = dock_icon_center(8);
            b.click(x, y).wait(500).click(181 + 60, 113 + 112).wait(300);
            let i = if name == "res1280" { 1 } else { 2 };
            b.click(181 + 188 + i * 112 + 52, 113 + 219).wait(1500);
            if name == "res1440click" {
                // The dock moved with the screen: Terminal's tile is now at (540, 856).
                b.click(540, 856)
                    .wait(600)
                    .typ("echo clicked at 1440x900")
                    .enter()
                    .wait(500);
            }
            if name == "resback" {
                // Settings stays where it was: 1024 x 768 is the first button.
                b.click(181 + 188 + 52, 113 + 219).wait(1500);
            }
        }
        // Overlays at 1440x900: Spaces, Today, Skim, the lock screen.
        "big-spaces" | "big-today" | "big-skim" | "big-lock" | "mid-today" | "mid-skim" => {
            let (w, h) = if name.starts_with("mid") {
                (1280, 800)
            } else {
                (1440, 900)
            };
            b.0.push(Step::Resolution(w, h));
            b.wait(600);
            match name {
                "big-spaces" => b.key(Key::F(3), Mods::NONE),
                "big-today" | "mid-today" => b.key(Key::F(4), Mods::NONE),
                "big-skim" | "mid-skim" => b.key(Key::Char(' '), Mods::META).wait(150).typ("o"),
                _ => b.key(Key::Char('q'), Mods::META_CTRL),
            };
            b.wait(900);
        }
        "bench1280" | "bench1440" => {
            let (w, h) = if name == "bench1280" {
                (1280, 800)
            } else {
                (1440, 900)
            };
            b.0.push(Step::Resolution(w, h));
            b.wait(800);
            b.0.push(Step::Bench);
        }
        // Welcome, from the logo menu (it doesn't open by itself under demo scripts).
        // Welcome buttons (window at (142, 40), body at (143, 73)).
        "welcome" | "welcome-snap" | "welcome-versions" | "welcome-peek" | "welcome-dark"
        | "welcome-spaces" | "welcome-dontshow" | "welcome-try" | "welcome-moo" => {
            if name == "welcome-try" || name == "welcome-moo" {
                // A package repo (scripts/wm-pkg-demo.sh serves one); moo2048 installed.
                let url = host_test_url();
                b.wait(300)
                    .typ(&alloc::format!("echo {url} > /etc/milk/repos"))
                    .enter()
                    .wait(300)
                    .typ("milk update")
                    .enter()
                    .wait(2000);
                if name == "welcome-moo" {
                    b.typ("milk install moo2048").enter().wait(3000);
                }
            }
            b.click(18, 14).wait(300).click(90, 72).wait(800);
            match name {
                "welcome-snap" => b.click(304, 281).wait(1500),
                "welcome-versions" => b.click(657, 281).wait(2500),
                "welcome-peek" => b.click(696, 419).wait(1500).click(777, 419).wait(1500),
                "welcome-dark" => b.click(300, 557).wait(1500),
                "welcome-spaces" => b.click(635, 557).wait(1200),
                "welcome-dontshow" => b.click(182, 614).wait(800),
                "welcome-try" => b.click(306, 419).wait(8000),
                "welcome-moo" => b.click(612, 419).wait(2000).click(777, 419).wait(1500),
                _ => &mut b,
            };
        }
        // Drag and drop from Files (window (96, 64); todo.txt is the second icon, at
        // (421, 150)). The Terminal is at (440, 270) 520 x 290.
        "dragterm" | "dragghost" => {
            b.wait(300).typ("cat ").wait(200);
            b.to(421, 150).wait(30).down().wait(30);
            for i in 1..=16 {
                b.to(421 + 18 * i, 150 + 19 * i).wait(16);
            }
            if name == "dragterm" {
                b.up().wait(300).enter().wait(800);
            } else {
                b.wait(400);
            }
        }
        "dragnotes" | "dragdockterm" => {
            // Onto the dock: Notes opens it; Terminal opens a tab in its folder.
            let (tx, ty) = dock_icon_center(if name == "dragnotes" { 3 } else { 1 });
            b.wait(300).to(421, 150).wait(30).down().wait(30);
            for i in 1..=16 {
                b.to(421 + (tx - 421) * i / 16, 150 + (ty - 150) * i / 16)
                    .wait(16);
            }
            b.up().wait(1500);
        }
        "dragpkg" => {
            // moo2048 installed (tile 10, after the Store): drop todo.txt on its tile.
            let url = host_test_url();
            b.wait(300)
                .typ(&alloc::format!("echo {url} > /etc/milk/repos"))
                .enter()
                .wait(300);
            b.typ("milk update").enter().wait(2000);
            b.typ("milk install moo2048").enter().wait(3000);
            b.typ("clear").enter().wait(3500);
            let (tx, ty) = dock_icon_center_of(10, 2);
            b.to(421, 150).wait(30).down().wait(30);
            for i in 1..=16 {
                b.to(421 + (tx - 421) * i / 16, 150 + (ty - 150) * i / 16)
                    .wait(16);
            }
            b.up().wait(2000);
            let (x, y) = dock_icon_center_of(1, 2);
            b.click(x, y).wait(400);
            b.typ("audit --event powerbox").enter().wait(600);
            b.typ("audit --event sandbox").enter().wait(800);
        }
        "dragpeek" => {
            // Peek (sandboxed, no fs) opens centred; drag it by its title bar so its body is
            // at the bottom right, then drop todo.txt on it: the drop is the grant.
            b.wait(300).typ("open /bin/peek").enter().wait(1500);
            let r = super::client::place(1, 522, 374);
            let (tx, ty) = (r.x + 258, r.y + 16);
            b.drag(tx, ty, tx + (392 - r.x), ty + (364 - r.y), 10)
                .wait(300);
            b.click(300, 80).wait(300);
            b.to(421, 150).wait(30).down().wait(30);
            for i in 1..=16 {
                b.to(421 + (800 - 421) * i / 16, 150 + (560 - 150) * i / 16)
                    .wait(16);
            }
            b.up().wait(1200);
            let (x, y) = dock_icon_center(1);
            b.click(x, y).wait(400);
            b.typ("audit --event powerbox").enter().wait(800);
        }
        // Notification centre: a few toasts from different apps, then click the clock.
        "notif" | "notifempty" | "notifclear" => {
            if name != "notifempty" {
                b.wait(300).typ("notify 'Milk is ready'").enter().wait(300);
                b.key(
                    Key::Char('3'),
                    Mods {
                        meta: true,
                        shift: true,
                        ..Mods::NONE
                    },
                )
                .wait(1200);
                b.typ("notify 'Tea is brewing'").enter().wait(300);
                b.key(Key::Char(' '), Mods::META)
                    .wait(150)
                    .typ("back up now")
                    .wait(200);
                b.enter().wait(800);
            }
            // The clock is the right end of the menu bar.
            b.click(super::theme::screen_w() - 30, 14).wait(600);
            if name == "notifclear" {
                let r = super::notif::rect();
                b.click(r.x + super::notif::W - 18 - 30, r.y + 20).wait(500);
            }
        }
        // Hover states: over a Welcome button, over a Settings section.
        "hover-welcome" => {
            b.click(18, 14).wait(300).click(90, 72).wait(800);
            b.glide(600, 500, 696, 419, 8).wait(300);
        }
        "hover-settings" => {
            let (x, y) = dock_icon_center(8);
            b.click(x, y).wait(500);
            b.glide(600, 400, 181 + 60, 113 + 44 + 2 * 28 + 12, 8)
                .wait(300);
        }
        // The Milk Store (a system app, /System/Applications/store): tile 9.
        "store" | "store-repo" => {
            if name == "store-repo" {
                // The toy repo scripts/wm-pkg-demo.sh serves.
                let url = host_test_url();
                b.wait(300)
                    .typ(&alloc::format!("echo {url} > /etc/milk/repos"))
                    .enter()
                    .wait(300);
            }
            let (x, y) = dock_icon_center(9);
            b.wait(300).click(x, y).wait(4000);
        }
        // The notification drawer (SPEC "Notification drawer"): Mary's four states.
        "drawer-arrive" | "drawer-open" | "drawer-keys" | "drawer-empty" | "drawer-mentions" => {
            if name != "drawer-empty" {
                b.wait(300)
                    .typ("notify Mum Photos from the weekend")
                    .enter()
                    .wait(300);
                if name != "drawer-arrive" {
                    b.typ("notify Gus @milk the allocator merged, build is green")
                        .enter()
                        .wait(300);
                    b.key(Key::Char(' '), Mods::META)
                        .wait(150)
                        .typ("back up now")
                        .wait(200);
                    b.enter().wait(500);
                    b.key(
                        Key::Char('3'),
                        Mods {
                            meta: true,
                            shift: true,
                            ..Mods::NONE
                        },
                    )
                    .wait(1200);
                    b.typ("notify Gus PR is up for review").enter().wait(400);
                }
            }
            if name == "drawer-arrive" {
                b.wait(600);
            } else {
                b.key(Key::Char('n'), Mods::CTRL).wait(500);
            }
            match name {
                "drawer-keys" => {
                    // Down twice, collapse the selected group, then hover a card (its x).
                    b.key(Key::Down, Mods::NONE).wait(150);
                    b.key(Key::Down, Mods::NONE).wait(150);
                    b.key(Key::Left, Mods::NONE).wait(300);
                    let r = super::notif::rect();
                    b.glide(600, 400, r.x + 150, r.y + 88 + 22 + 30, 8)
                        .wait(400);
                }
                "drawer-mentions" => {
                    b.key(Key::Tab, Mods::NONE).wait(400);
                }
                "drawer-empty" => {
                    // Do Not Disturb on: the switch at the bottom.
                    let r = super::notif::rect();
                    b.click(r.x + r.w - 12 - 36, r.y + r.h - 12 - 22).wait(500);
                }
                _ => {}
            }
        }
        // Tabs everywhere (SPEC Tabs). Files is at (96, 64) 600 wide, the Terminal at
        // (440, 270): drag the Terminal by its title bar onto Files' title bar.
        "tabs-drop" | "tabs-merge" | "tabs-split" | "tabs-mixed" | "tabs-plus" | "tabs-keys"
        | "tabs-close" => {
            b.wait(300).to(700, 286).wait(30).down().wait(30);
            for i in 1..=14 {
                b.to(700 + (330 - 700) * i / 14, 286 + (80 - 286) * i / 14)
                    .wait(16);
            }
            b.wait(200);
            if name == "tabs-drop" {
                b.wait(300);
            } else {
                b.up().wait(500);
                // Two tabs now; Files' strip is 38 high, tabs 160 wide from x84.
                match name {
                    "tabs-split" => {
                        // Pull the Terminal tab (the second) down out of the strip.
                        b.drag(96 + 84 + 166 + 60, 64 + 22, 560, 420, 12).wait(500);
                    }
                    "tabs-mixed" | "tabs-keys" | "tabs-close" => {
                        // Notes too (it opens at (120, 96)): drag it onto the strip.
                        let (x, y) = dock_icon_center(3);
                        b.click(x, y).wait(600);
                        b.drag(340, 112, 330, 80, 12).wait(500);
                        if name == "tabs-keys" {
                            b.key(
                                Key::Char('['),
                                Mods {
                                    meta: true,
                                    shift: true,
                                    ..Mods::NONE
                                },
                            )
                            .wait(400);
                        }
                        if name == "tabs-close" {
                            b.key(Key::Char('w'), Mods::META).wait(500);
                        }
                    }
                    "tabs-plus" => {
                        // The + after the tabs: Skim, then Music joins as a tab.
                        b.click(96 + 84 + 2 * 166 + 4 + 14, 64 + 22).wait(400);
                        b.typ("music").wait(200).enter().wait(800);
                    }
                    _ => {}
                }
            }
        }
        "tabs-terminal" => {
            // The Terminal alone with two shells: its tabs are the window's strip.
            b.wait(300).key(Key::Char('t'), Mods::META).wait(600);
            b.typ("echo second shell").enter().wait(300);
        }
        "logomenu" => {
            b.click(18, 14).wait(300);
        }
        "lockmenu" => {
            // The logo's system menu > Lock Screen.
            b.click(18, 14).wait(300).click(80, 220).wait(700);
        }
        "lockprefs" => {
            // Settings > Display & Lock > 5 min.
            let (x, y) = dock_icon_center(8);
            b.click(x, y).wait(500).click(181 + 60, 113 + 112).wait(300);
            b.click(181 + 188 + 2 * 82 + 38, 113 + 325).wait(400);
        }
        "idlelock" => {
            // Settings > Display & Lock > 1 min, then a minute of (pretend) idleness.
            let (x, y) = dock_icon_center(8);
            b.click(x, y).wait(500).click(181 + 60, 113 + 112).wait(300);
            b.click(181 + 188 + 82 + 38, 113 + 325).wait(300);
            b.0.push(Step::Idle(61));
            b.wait(800);
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
            b.typ("cowsay hello milk").enter().wait(1500);
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

/// The test web server for the browse2 / browse3 demos: the Mac's 127.0.0.1 as seen from
/// the guest (10.0.2.2, WM_HOSTNET=1), on the port in fw_cfg `opt/milkos/testport`.
/// Where Paint's window is now vs. where the paint demos were written for (192, 112).
fn paint_offset() -> (i32, i32) {
    let r = super::client::place(1, 562, 414);
    (r.x - 192, r.y - 112)
}

fn host_test_url() -> String {
    let port = fwcfg::find_file("opt/milkos/testport")
        .and_then(|f| {
            let mut buf = alloc::vec![0u8; 8];
            let n = (f.size as usize).min(8);
            fwcfg::read_file(f, &mut buf[..n]).then(|| {
                core::str::from_utf8(&buf[..n])
                    .unwrap_or("8000")
                    .trim_matches(char::from(0))
                    .trim()
                    .parse::<u16>()
                    .unwrap_or(8000)
            })
        })
        .unwrap_or(8000);
    alloc::format!("http://10.0.2.2:{port}/")
}

/// Booted for a demo (any `opt/milkos/demo`, even "none"): the desktop then skips first-boot
/// things like the Welcome window, whose clicks the demos don't expect. `firstboot` is the
/// demo that wants them.
pub static DEMO_BOOT: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
/// The `firstboot` demo: boot like a user would (it ends once the desktop is up).
pub static FIRSTBOOT: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

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
    DEMO_BOOT.store(name != "firstboot", core::sync::atomic::Ordering::Relaxed);
    FIRSTBOOT.store(name == "firstboot", core::sync::atomic::Ordering::Relaxed);
    if name == "none" || name == "firstboot" || name.is_empty() {
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
    // dark-<demo>: the same demo in Chocolate milk (the polish pass checks both themes).
    let (inner, dark) = match name.strip_prefix("dark-") {
        Some(n) => (n, true),
        None => (name, false),
    };
    let steps = if inner == "none" {
        Some(alloc::vec![Step::Log("done")])
    } else {
        build(inner)
    };
    let Some(mut steps) = steps else {
        kprintln!("[demo] unknown demo '{name}'");
        return None;
    };
    if dark {
        steps.insert(0, Step::Theme(true));
        steps.insert(1, Step::Wait(400));
    }
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
                Step::Idle(s) => d.pretend_idle(*s),
                Step::Theme(dark) => {
                    let mut p = super::prefs::get();
                    p.dark = *dark;
                    super::request(super::Request::Prefs(p));
                }
                Step::Resolution(w, h) => {
                    d.set_resolution(*w, *h);
                }
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
