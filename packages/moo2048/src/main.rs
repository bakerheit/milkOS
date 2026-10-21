//! moo2048: 2048 in a desktop window (user/gui). Arrows or WASD slide, n starts over,
//! closing the window quits. Tiles are named after what milk becomes: 2 drop, 4 sip,
//! 8 glass, ... 2048 the whole cow.
//!   moo2048              play (needs the desktop)
//!   moo2048 --selftest   check the slide/merge rules and play a random game, no window

#![no_std]
#![no_main]

extern crate alloc;

use gui::{Event, Key, Window};
use rt::{eprintln, println, sys};

rt::main!(main);

const N: usize = 4;
type Board = [[u32; N]; N];

const NAMES: [&str; 12] = [
    "", "drop", "sip", "glass", "jug", "churn", "pail", "cream", "butter", "cheese", "herd",
    "COW",
];

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// Slide one row towards index 0, merging equal neighbours once. Returns points scored.
fn slide_row(row: &mut [u32; N]) -> u32 {
    let tiles: alloc::vec::Vec<u32> = row.iter().copied().filter(|&v| v != 0).collect();
    let mut out = [0u32; N];
    let (mut i, mut k, mut pts) = (0, 0, 0);
    while i < tiles.len() {
        if i + 1 < tiles.len() && tiles[i] == tiles[i + 1] {
            out[k] = tiles[i] * 2;
            pts += out[k];
            i += 2;
        } else {
            out[k] = tiles[i];
            i += 1;
        }
        k += 1;
    }
    *row = out;
    pts
}

#[derive(Clone, Copy)]
enum Dir {
    Left,
    Right,
    Up,
    Down,
}

/// Apply a move; returns (changed, points).
fn apply(b: &mut Board, d: Dir) -> (bool, u32) {
    let before = *b;
    let mut pts = 0;
    for k in 0..N {
        let mut row = [0u32; N];
        for (i, v) in row.iter_mut().enumerate() {
            *v = match d {
                Dir::Left => b[k][i],
                Dir::Right => b[k][N - 1 - i],
                Dir::Up => b[i][k],
                Dir::Down => b[N - 1 - i][k],
            };
        }
        pts += slide_row(&mut row);
        for (i, &v) in row.iter().enumerate() {
            match d {
                Dir::Left => b[k][i] = v,
                Dir::Right => b[k][N - 1 - i] = v,
                Dir::Up => b[i][k] = v,
                Dir::Down => b[N - 1 - i][k] = v,
            }
        }
    }
    (*b != before, pts)
}

fn spawn(b: &mut Board, rng: &mut Rng) {
    let empty: alloc::vec::Vec<(usize, usize)> = (0..N * N)
        .map(|i| (i / N, i % N))
        .filter(|&(r, c)| b[r][c] == 0)
        .collect();
    if empty.is_empty() {
        return;
    }
    let (r, c) = empty[(rng.next() % empty.len() as u64) as usize];
    b[r][c] = if rng.next() % 10 == 0 { 4 } else { 2 };
}

fn can_move(b: &Board) -> bool {
    [Dir::Left, Dir::Right, Dir::Up, Dir::Down]
        .iter()
        .any(|&d| apply(&mut b.clone(), d).0)
}

fn selftest() -> i32 {
    let cases: [([u32; N], [u32; N], u32); 5] = [
        ([2, 2, 2, 2], [4, 4, 0, 0], 8),
        ([2, 0, 2, 4], [4, 4, 0, 0], 4),
        ([4, 4, 8, 0], [8, 8, 0, 0], 8),
        ([2, 4, 8, 16], [2, 4, 8, 16], 0),
        ([0, 0, 0, 2], [2, 0, 0, 0], 0),
    ];
    for (inp, want, wpts) in cases {
        let mut r = inp;
        let p = slide_row(&mut r);
        if r != want || p != wpts {
            eprintln!("moo2048: slide {inp:?} gave {r:?} (+{p}), want {want:?} (+{wpts})");
            return 1;
        }
    }
    let mut rng = Rng(0x2048_6d6f6f);
    let mut b: Board = [[0; N]; N];
    spawn(&mut b, &mut rng);
    spawn(&mut b, &mut rng);
    let (mut score, mut moves) = (0, 0);
    while can_move(&b) && moves < 5000 {
        let d = [Dir::Left, Dir::Up, Dir::Right, Dir::Down][(rng.next() % 4) as usize];
        let (changed, p) = apply(&mut b, d);
        if changed {
            score += p;
            moves += 1;
            spawn(&mut b, &mut rng);
        }
    }
    let max = b.iter().flatten().copied().max().unwrap_or(0);
    println!(
        "moo2048: selftest ok (5 slide rules; random game: {moves} moves, score {score}, best tile {max} = {})",
        NAMES[max.trailing_zeros() as usize % NAMES.len()]
    );
    0
}

fn colour(v: u32) -> gui::Color {
    match v {
        0 => gui::OAT,
        2 => gui::MILK,
        4 => gui::CREAM,
        8 => gui::HONEY,
        16 => gui::STRAW,
        32 => gui::MATCHA,
        64 => gui::BLUE,
        128 => gui::LAV,
        256 => gui::LATTE,
        _ => gui::COCOA,
    }
}

const CELL: i32 = 96;
const GAP: i32 = 12;
const TOP: i32 = 56;
const WIDTH: i32 = GAP + N as i32 * (CELL + GAP);
const HEIGHT: i32 = TOP + WIDTH;

fn draw(w: &mut Window, b: &Board, score: u32, over: bool) {
    w.fill(0, 0, WIDTH, HEIGHT, gui::MILK);
    w.bold(GAP, 16, "moo2048", gui::ESPRESSO);
    let s = alloc::format!("score {score}{}", if over { "  - no moves left (n: new game)" } else { "" });
    w.text(GAP + 96, 18, &s, gui::COCOA);
    w.rrect(GAP / 2, TOP - GAP / 2, WIDTH - GAP, WIDTH - GAP, 12, gui::SAND);
    for (r, row) in b.iter().enumerate() {
        for (c, &v) in row.iter().enumerate() {
            let x = GAP + c as i32 * (CELL + GAP);
            let y = TOP + r as i32 * (CELL + GAP);
            w.rrect(x, y, CELL, CELL, 10, colour(v));
            if v != 0 {
                let num = alloc::format!("{v}");
                let fg = if v >= 8 { gui::MILK } else { gui::ESPRESSO };
                let nw = gui::str_width(&num, 2);
                w.text_x(x + (CELL - nw) / 2, y + 26, &num, fg, 2);
                let name = NAMES[v.trailing_zeros() as usize % NAMES.len()];
                let tw = gui::str_width(name, 1);
                w.text(x + (CELL - tw) / 2, y + 66, name, fg);
            }
        }
    }
    w.commit();
}

fn main(args: &rt::Args) -> i32 {
    if args.get(1) == Some("--selftest") {
        return selftest();
    }
    let mut w = match Window::new("moo2048", WIDTH, HEIGHT) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("moo2048: no window ({e}); it needs the desktop (try --selftest)");
            return 1;
        }
    };
    let mut rng = Rng(sys::clock_ns(sys::CLOCK_MONOTONIC) | 1);
    let new_board = |rng: &mut Rng| {
        let mut b: Board = [[0; N]; N];
        spawn(&mut b, rng);
        spawn(&mut b, rng);
        b
    };
    let mut b = new_board(&mut rng);
    let mut score = 0;
    draw(&mut w, &b, score, false);
    loop {
        let Some(ev) = w.event(1000) else { continue };
        let d = match ev {
            Event::Close => return 0,
            Event::Key(Key::Left | Key::Char('a'), _) => Some(Dir::Left),
            Event::Key(Key::Right | Key::Char('d'), _) => Some(Dir::Right),
            Event::Key(Key::Up | Key::Char('w'), _) => Some(Dir::Up),
            Event::Key(Key::Down | Key::Char('s'), _) => Some(Dir::Down),
            Event::Key(Key::Char('n'), _) => {
                b = new_board(&mut rng);
                score = 0;
                None
            }
            _ => None,
        };
        if let Some(d) = d {
            let (changed, p) = apply(&mut b, d);
            if changed {
                score += p;
                spawn(&mut b, &mut rng);
            }
        }
        draw(&mut w, &b, score, !can_move(&b));
    }
}
