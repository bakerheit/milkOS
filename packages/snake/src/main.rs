//! snake: the classic, in the terminal. Raw mode (TCGETS/TCSETS), ANSI escapes for
//! drawing, ppoll on stdin for the game clock.
//!   snake              play: arrows or WASD, p pause, q quit
//!   snake --demo [N]   autopilot for N ticks (default 300), no input, no delays; prints
//!                      the final board and "snake: demo over, length L, score S"

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;
use rt::net::{POLLIN, PollFd};
use rt::{print, println, sys};

rt::main!(main);

const W: i32 = 32;
const H: i32 = 16;
const TCGETS: usize = 0x5401;
const TCSETS: usize = 0x5402;
const ISIG: u32 = 0o1;
const ICANON: u32 = 0o2;
const ECHO: u32 = 0o10;
const ICRNL: u32 = 0o400;

fn ioctl(cmd: usize, buf: &mut [u8]) -> isize {
    // SAFETY: buf is valid for the termios struct the ioctl reads or writes.
    unsafe { sys::syscall(29, [0, cmd, buf.as_mut_ptr() as usize, 0, 0, 0]) }
}

/// Raw mode on fd 0; returns the old termios to restore.
fn raw_mode() -> Option<[u8; 60]> {
    let mut t = [0u8; 60];
    if ioctl(TCGETS, &mut t) < 0 {
        return None;
    }
    let old = t;
    let word = |t: &[u8], k: usize| u32::from_le_bytes([t[k], t[k + 1], t[k + 2], t[k + 3]]);
    let (iflag, lflag) = (word(&t, 0), word(&t, 12));
    t[0..4].copy_from_slice(&(iflag & !ICRNL).to_le_bytes());
    t[12..16].copy_from_slice(&(lflag & !(ICANON | ECHO | ISIG)).to_le_bytes());
    (ioctl(TCSETS, &mut t) >= 0).then_some(old)
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: i32) -> i32 {
        (self.next() % n as u64) as i32
    }
}

struct Game {
    body: Vec<(i32, i32)>, // head first
    dir: (i32, i32),
    food: (i32, i32),
    score: u32,
    over: bool,
    rng: Rng,
}

impl Game {
    fn new(seed: u64) -> Game {
        let mut g = Game {
            body: alloc::vec![(W / 2, H / 2), (W / 2 - 1, H / 2), (W / 2 - 2, H / 2)],
            dir: (1, 0),
            food: (0, 0),
            score: 0,
            over: false,
            rng: Rng(seed | 1),
        };
        g.place_food();
        g
    }
    fn place_food(&mut self) {
        loop {
            let f = (self.rng.below(W), self.rng.below(H));
            if !self.body.contains(&f) {
                self.food = f;
                return;
            }
        }
    }
    fn turn(&mut self, d: (i32, i32)) {
        if (d.0 + self.dir.0, d.1 + self.dir.1) != (0, 0) {
            self.dir = d;
        }
    }
    fn blocked(&self, p: (i32, i32)) -> bool {
        p.0 < 0 || p.1 < 0 || p.0 >= W || p.1 >= H || self.body[..self.body.len() - 1].contains(&p)
    }
    fn step(&mut self) {
        let h = self.body[0];
        let n = (h.0 + self.dir.0, h.1 + self.dir.1);
        if self.blocked(n) {
            self.over = true;
            return;
        }
        self.body.insert(0, n);
        if n == self.food {
            self.score += 10;
            if self.body.len() as i32 >= W * H {
                self.over = true;
                return;
            }
            self.place_food();
        } else {
            self.body.pop();
        }
    }
    /// Autopilot: head for the food, never into a wall or itself if it can help it.
    fn autopilot(&mut self) {
        let h = self.body[0];
        let mut best = None;
        for d in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            if (d.0 + self.dir.0, d.1 + self.dir.1) == (0, 0) {
                continue;
            }
            let n = (h.0 + d.0, h.1 + d.1);
            if self.blocked(n) {
                continue;
            }
            let dist = (n.0 - self.food.0).abs() + (n.1 - self.food.1).abs();
            if best.is_none_or(|(bd, _)| dist < bd) {
                best = Some((dist, d));
            }
        }
        if let Some((_, d)) = best {
            self.dir = d;
        }
    }
    fn render(&self, out: &mut String, title: &str) {
        out.push_str("\x1b[H");
        let _ = writeln!(out, " {title}  score {:<5} length {:<4}\r", self.score, self.body.len());
        out.push_str(" +");
        for _ in 0..W {
            out.push('-');
        }
        out.push_str("+\r\n");
        for y in 0..H {
            out.push_str(" |");
            for x in 0..W {
                let c = if (x, y) == self.body[0] {
                    '@'
                } else if self.body.contains(&(x, y)) {
                    'o'
                } else if (x, y) == self.food {
                    '*'
                } else {
                    ' '
                };
                out.push(c);
            }
            out.push_str("|\r\n");
        }
        out.push_str(" +");
        for _ in 0..W {
            out.push('-');
        }
        out.push_str("+\r\n");
    }
}

fn seed() -> u64 {
    let mut b = [0u8; 8];
    // SAFETY: getrandom into an 8-byte buffer.
    unsafe { sys::syscall(278, [b.as_mut_ptr() as usize, 8, 0, 0, 0, 0]) };
    u64::from_le_bytes(b) ^ sys::clock_ns(sys::CLOCK_MONOTONIC)
}

fn demo(ticks: u32) -> i32 {
    let mut g = Game::new(0x6d696c6b);
    let mut t = 0;
    while t < ticks && !g.over {
        g.autopilot();
        g.step();
        t += 1;
    }
    let mut s = String::new();
    g.render(&mut s, "snake (demo)");
    print!("{}", s.replace("\x1b[H", ""));
    println!(
        "snake: demo over after {t} ticks, length {}, score {}{}",
        g.body.len(),
        g.score,
        if g.over { " (crashed)" } else { "" }
    );
    0
}

fn main(args: &rt::Args) -> i32 {
    if args.get(1) == Some("--demo") {
        return demo(args.get(2).and_then(|n| n.parse().ok()).unwrap_or(300));
    }
    let Some(old) = raw_mode() else {
        println!("snake: stdin is not a terminal (try: snake --demo)");
        return 1;
    };
    let mut g = Game::new(seed());
    let mut paused = false;
    let mut out = String::new();
    print!("\x1b[2J\x1b[?25l");
    let mut esc = 0u8;
    loop {
        out.clear();
        g.render(&mut out, if paused { "snake (paused)" } else { "snake" });
        out.push_str(" arrows/WASD move, p pause, q quit\r\n");
        print!("{out}");
        if g.over {
            break;
        }
        let tick = 160 - (g.body.len() as u64 * 2).min(90);
        let deadline = sys::clock_ns(sys::CLOCK_MONOTONIC) + tick * 1_000_000;
        let mut quit = false;
        loop {
            let now = sys::clock_ns(sys::CLOCK_MONOTONIC);
            if now >= deadline {
                break;
            }
            let mut fds = [PollFd {
                fd: 0,
                events: POLLIN,
                revents: 0,
            }];
            if rt::net::poll(&mut fds, Some((deadline - now) / 1_000_000 + 1)).unwrap_or(0) == 0 {
                continue;
            }
            let mut b = [0u8; 16];
            let n = sys::read(0, &mut b).unwrap_or(0);
            for &c in &b[..n] {
                match (esc, c) {
                    (0, 0x1b) => esc = 1,
                    (1, b'[') => esc = 2,
                    (2, b'A') | (0, b'w') => {
                        esc = 0;
                        g.turn((0, -1))
                    }
                    (2, b'B') | (0, b's') => {
                        esc = 0;
                        g.turn((0, 1))
                    }
                    (2, b'C') | (0, b'd') => {
                        esc = 0;
                        g.turn((1, 0))
                    }
                    (2, b'D') | (0, b'a') => {
                        esc = 0;
                        g.turn((-1, 0))
                    }
                    (0, b'p') => paused = !paused,
                    (0, b'q') | (0, 3) => quit = true,
                    _ => esc = 0,
                }
            }
        }
        if quit {
            break;
        }
        if !paused {
            g.step();
        }
    }
    let mut t = old;
    ioctl(TCSETS, &mut t);
    print!("\x1b[?25h");
    println!("snake: game over, length {}, score {}", g.body.len(), g.score);
    0
}
