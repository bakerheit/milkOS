//! Music: plays WAVs from ~/Music and /System/Library/Sounds through /dev/audio.
//!
//! The player is global (one "music" kernel thread, the state in [`PLAYER`]); the window is
//! a view of it, and closing the window stops playback. The thread decodes a whole track
//! (`audio::wav::decode`: 16-bit PCM, any rate, mono or stereo, converted to 48 kHz stereo),
//! then writes it to /dev/audio 20 ms at a time. Writes block once the mixer's 250 ms ring
//! is full, so the device paces us; what is on screen is what was written minus that
//! latency. Each chunk also gets a 12-band level reading (Goertzel probes on every
//! semitone, half an octave per band), shown with the same delay as a small visualizer.
//!
//! Without a sound device the thread paces itself with the timer and plays silently, so the
//! app still works (and the demos run) on machines started without one.
//!
//! ~/Music is seeded on first boot with the tunes in /System/Library/Music
//! (scripts/gen_music.py).

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::fs::{self, InodeType};
use crate::gfx::Rect;
use crate::sched::{self, WaitQueue};
use crate::sync::SpinLock;
use crate::timer;

use super::super::event::Key;
use super::super::icons::{self, Icon};
use super::super::paint::{Painter, hit};
use super::super::theme::*;
use super::super::window::{KeyInput, PointerInput, PointerKind, WindowContents};
use super::fsx;

pub const LIBRARY: &str = "/home/milk/Music";
const SEEDS: &str = "/System/Library/Music";
const SOUNDS: &str = crate::audio::SOUND_DIR;
const SEEDED: &str = "/home/milk/.config/milk/music-seeded";

const RATE: u64 = 48_000;
/// Frames per write (20 ms).
const CHUNK: usize = 960;
/// Written but not yet heard: the mixer's 250 ms ring plus the device's 40 ms.
const LATENCY: u64 = RATE * 290 / 1000;
const LAT_CHUNKS: usize = (LATENCY as usize).div_ceil(CHUNK);
pub const BANDS: usize = 12;

// ---- the library -----------------------------------------------------------------------------

#[derive(Clone)]
pub struct Track {
    pub path: String,
    pub title: String,
    /// Length in 48 kHz frames (from the WAV header).
    pub frames: u64,
    pub system: bool,
}

/// "morning-pasture.wav" -> "Morning Pasture".
pub fn pretty_title(name: &str) -> String {
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
    let mut out = String::new();
    let mut up = true;
    for c in stem.chars() {
        let c = if c == '-' || c == '_' { ' ' } else { c };
        if up {
            out.extend(c.to_uppercase());
        } else {
            out.push(c);
        }
        up = c == ' ';
    }
    out
}

fn le16(b: &[u8], o: usize) -> u64 {
    b.get(o..o + 2)
        .map_or(0, |x| u16::from_le_bytes([x[0], x[1]]) as u64)
}
fn le32(b: &[u8], o: usize) -> u64 {
    b.get(o..o + 4)
        .map_or(0, |x| u32::from_le_bytes([x[0], x[1], x[2], x[3]]) as u64)
}

/// A WAV's length in 48 kHz frames, from its header (first 256 bytes).
fn wav_frames(path: &str) -> Option<u64> {
    let node = fs::lookup(path).ok()?;
    let mut b = [0u8; 256];
    let n = node.read_at(0, &mut b).ok()?;
    let b = &b[..n];
    if b.get(0..4)? != b"RIFF" || b.get(8..12)? != b"WAVE" {
        return None;
    }
    let (mut ch, mut rate, mut bits) = (0, 0, 0);
    let mut k = 12;
    while k + 8 <= b.len() {
        let len = le32(b, k + 4);
        match &b[k..k + 4] {
            b"fmt " => {
                ch = le16(b, k + 10);
                rate = le32(b, k + 12);
                bits = le16(b, k + 22);
            }
            b"data" => {
                let size = if len == 0 || len == 0xffff_ffff {
                    node.stat().size.saturating_sub(k as u64 + 8)
                } else {
                    len
                };
                let per = ch * bits / 8;
                if per == 0 || rate == 0 {
                    return None;
                }
                return Some(size / per * RATE / rate);
            }
            _ => {}
        }
        k += 8 + len as usize + (len as usize & 1);
    }
    None
}

fn scan(dir: &str, system: bool, out: &mut Vec<Track>) {
    let Ok(mut entries) = fs::readdir(dir) else {
        return;
    };
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    for e in entries {
        if e.kind != InodeType::File || !e.name.to_ascii_lowercase().ends_with(".wav") {
            continue;
        }
        let path = format!("{dir}/{}", e.name);
        if let Some(frames) = wav_frames(&path) {
            out.push(Track {
                title: pretty_title(&e.name),
                path,
                frames,
                system,
            });
        }
    }
}

/// ~/Music, then the system sounds.
pub fn library() -> Vec<Track> {
    let mut v = Vec::new();
    scan(LIBRARY, false, &mut v);
    scan(SOUNDS, true, &mut v);
    v
}

/// Copy the bundled tunes into ~/Music once (a marker file remembers, so a tune you delete
/// stays deleted). Runs in its own thread: a couple of MB of writes shouldn't hold up the
/// first frame.
pub fn seed_library() {
    if fs::stat(SEEDED).is_ok() {
        return;
    }
    drop(sched::spawn("music-seed", || {
        let _ = fs::mkdir_all(LIBRARY, 0o755);
        if let Ok(entries) = fs::readdir(SEEDS) {
            for e in entries {
                let dst = format!("{LIBRARY}/{}", e.name);
                if fs::stat(&dst).is_err()
                    && let Ok(b) = fs::read_file(&format!("{SEEDS}/{}", e.name))
                {
                    let _ = fs::write_file(&dst, &b);
                }
            }
        }
        let _ = fs::mkdir_all("/home/milk/.config/milk", 0o755);
        let _ = fs::write_file(SEEDED, b"1\n");
    }));
}

// ---- the player ------------------------------------------------------------------------------

struct Player {
    /// The track loaded (or to load), and the rest of the play queue.
    track: Option<Track>,
    queue: Vec<Track>,
    playing: bool,
    /// Frames of the current track handed to /dev/audio.
    written: u64,
    /// Bumped when the track changes or we seek: the thread drops what it was doing.
    generation: u64,
    seek: Option<u64>,
    /// Band levels (0..=255) per chunk written, newest last.
    levels: VecDeque<[u8; BANDS]>,
    /// No /dev/audio: playing silently, paced by the timer.
    silent: bool,
    error: Option<String>,
}

static PLAYER: SpinLock<Player> = SpinLock::new(Player {
    track: None,
    queue: Vec::new(),
    playing: false,
    written: 0,
    generation: 0,
    seek: None,
    levels: VecDeque::new(),
    silent: false,
    error: None,
});
static WQ: WaitQueue = WaitQueue::new();
static STARTED: AtomicBool = AtomicBool::new(false);

/// What the window shows.
#[derive(Clone)]
pub struct Now {
    pub track: Option<Track>,
    pub playing: bool,
    /// Heard so far, in frames.
    pub pos: u64,
    pub levels: [u8; BANDS],
    pub silent: bool,
    pub error: Option<String>,
}

pub fn now() -> Now {
    let p = PLAYER.lock();
    let lat = if p.silent || !p.playing { 0 } else { LATENCY };
    let pos = p.written.saturating_sub(lat);
    let levels = if p.playing && !p.levels.is_empty() {
        let i = p.levels.len().saturating_sub(1 + LAT_CHUNKS);
        p.levels[i]
    } else {
        [0; BANDS]
    };
    Now {
        track: p.track.clone(),
        playing: p.playing,
        pos,
        levels,
        silent: p.silent,
        error: p.error.clone(),
    }
}

fn start_thread() {
    if !STARTED.swap(true, Ordering::AcqRel) {
        drop(sched::spawn_prio(
            "music",
            sched::Priority::Normal,
            player_thread,
        ));
    }
}

/// Play `t` now, then `queue` in order.
pub fn play(t: Track, queue: Vec<Track>) {
    start_thread();
    {
        let mut p = PLAYER.lock();
        p.track = Some(t);
        p.queue = queue;
        p.playing = true;
        p.written = 0;
        p.seek = None;
        p.generation += 1;
        p.levels.clear();
        p.error = None;
    }
    WQ.wake_all();
}

/// Play a file by path (Files, Skim, `open x.wav`), then the rest of its folder.
pub fn play_path(path: &str) {
    let lib = library();
    let i = lib.iter().position(|t| t.path == path);
    match i {
        Some(i) => {
            let rest = lib[i + 1..]
                .iter()
                .filter(|t| t.system == lib[i].system)
                .cloned()
                .collect();
            play(lib[i].clone(), rest);
        }
        None => {
            if let Some(frames) = wav_frames(path) {
                play(
                    Track {
                        path: String::from(path),
                        title: pretty_title(fsx::basename(path)),
                        frames,
                        system: false,
                    },
                    Vec::new(),
                );
            }
        }
    }
}

pub fn toggle() {
    let mut p = PLAYER.lock();
    if p.track.is_none() {
        return;
    }
    p.playing = !p.playing;
    if p.playing && p.written >= p.track.as_ref().map_or(0, |t| t.frames) {
        p.written = 0;
        p.generation += 1;
    }
    drop(p);
    WQ.wake_all();
}

pub fn stop() {
    let mut p = PLAYER.lock();
    p.playing = false;
    p.track = None;
    p.queue.clear();
    p.written = 0;
    p.generation += 1;
    p.levels.clear();
}

pub fn seek(frames: u64) {
    let mut p = PLAYER.lock();
    if let Some(t) = &p.track {
        p.seek = Some(frames.min(t.frames));
    }
    drop(p);
    WQ.wake_all();
}

/// cos for the Goertzel coefficients (no libm): range-reduced Taylor series.
fn cos(x: f64) -> f64 {
    let pi = core::f64::consts::PI;
    let mut x = x % (2.0 * pi);
    if x > pi {
        x -= 2.0 * pi;
    }
    let (mut term, mut sum) = (1.0, 1.0);
    for k in 1..12 {
        term *= -x * x / ((2 * k - 1) as f64 * (2 * k) as f64);
        sum += term;
    }
    sum
}

/// Goertzel coefficients: every semitone from C2 for 6 octaves, at 12 kHz (we decimate by 4).
fn coefficients() -> Vec<f64> {
    (0..BANDS * 6)
        .map(|k| {
            let f = 65.406 * pow2(k as f64 / 12.0);
            2.0 * cos(2.0 * core::f64::consts::PI * f / 12_000.0)
        })
        .collect()
}

fn pow2(x: f64) -> f64 {
    // 2^x = 2^int * e^(frac ln 2), the second by its series.
    let i = x as i32;
    let f = (x - i as f64) * core::f64::consts::LN_2;
    let (mut term, mut sum) = (1.0, 1.0);
    for k in 1..12 {
        term *= f / k as f64;
        sum += term;
    }
    sum * (1u64 << i) as f64
}

/// log2 of a positive number, roughly (exponent plus a linear mantissa).
fn log2(x: f64) -> f64 {
    let b = x.to_bits();
    let e = ((b >> 52) & 0x7ff) as f64 - 1023.0;
    let m = f64::from_bits((b & 0x000f_ffff_ffff_ffff) | 0x3ff0_0000_0000_0000);
    e + (m - 1.0)
}

/// 12 band levels (0..=255) of a chunk of interleaved stereo.
fn bands(coef: &[f64], pcm: &[i16]) -> [u8; BANDS] {
    // Mono, decimated by 4 (average of 4 frames: a cheap low-pass too).
    let mono: Vec<f64> = pcm
        .as_chunks::<8>()
        .0
        .iter()
        .map(|c| c.iter().map(|&s| s as f64).sum::<f64>() / 8.0)
        .collect();
    let n = mono.len().max(1) as f64;
    let mut out = [0u8; BANDS];
    for (b, o) in out.iter_mut().enumerate() {
        let mut best = 0.0f64;
        for &c in &coef[b * 6..b * 6 + 6] {
            let (mut s1, mut s2) = (0.0, 0.0);
            for &x in &mono {
                let s = x + c * s1 - s2;
                s2 = s1;
                s1 = s;
            }
            best = best.max(s1 * s1 + s2 * s2 - c * s1 * s2);
        }
        // Full scale: (32768 * n / 2)^2. Show -42..-12 dBFS (3 dB per log2 step).
        let full = log2(32768.0 * 32768.0 * n * n / 4.0);
        let v = (log2(best.max(1.0)) - (full - 14.0)) / 10.0;
        *o = (v.clamp(0.0, 1.0) * 255.0) as u8;
    }
    out
}

fn player_thread() {
    let audio = fs::lookup("/dev/audio").ok();
    PLAYER.lock().silent = audio.is_none();
    let coef = coefficients();
    let mut loaded: Option<(String, Vec<i16>)> = None;
    let mut bytes: Vec<u8> = Vec::with_capacity(CHUNK * 4);
    loop {
        let (want, generation, track) = {
            let mut p = PLAYER.lock();
            if let Some(s) = p.seek.take() {
                p.written = s;
                p.generation += 1;
                p.levels.clear();
            }
            (p.playing, p.generation, p.track.clone())
        };
        if track.is_none() {
            loaded = None; // stopped: give the samples back
        }
        let Some(track) = track.filter(|_| want) else {
            WQ.wait_until_timeout(|| PLAYER.lock().playing, 1_000_000_000);
            continue;
        };
        // (Re)load when the track changed; a seek keeps the samples.
        if loaded.as_ref().is_none_or(|(path, _)| *path != track.path) {
            let pcm = fs::read_file(&track.path)
                .ok()
                .and_then(|b| crate::audio::wav::decode(&b));
            match pcm {
                Some(pcm) => loaded = Some((track.path.clone(), pcm)),
                None => {
                    let mut p = PLAYER.lock();
                    p.error = Some(format!("Can't play {}", track.title));
                    p.playing = false;
                    continue;
                }
            }
        }
        let Some((_, pcm)) = loaded.as_ref() else {
            continue;
        };
        let total = pcm.len() as u64 / 2;
        let pos = PLAYER.lock().written;
        if pos >= total {
            // Next in the queue, or stop at the end (back at the start, paused).
            let mut p = PLAYER.lock();
            if p.generation == generation {
                if p.queue.is_empty() {
                    p.playing = false;
                    p.written = 0;
                } else {
                    let next = p.queue.remove(0);
                    p.track = Some(next);
                    p.written = 0;
                    p.generation += 1;
                    loaded = None;
                }
                p.levels.clear();
            }
            continue;
        }
        let end = (pos + CHUNK as u64).min(total);
        let chunk = &pcm[pos as usize * 2..end as usize * 2];
        let lv = bands(&coef, chunk);
        bytes.clear();
        for s in chunk {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        match &audio {
            Some(dev) => {
                let _ = dev.write_at(0, &bytes);
            }
            None => {
                let ns = (end - pos) * 1_000_000_000 / RATE;
                WQ.wait_until_timeout(|| false, ns);
            }
        }
        let mut p = PLAYER.lock();
        if p.generation == generation && p.written == pos {
            p.written = end;
            p.levels.push_back(lv);
            while p.levels.len() > LAT_CHUNKS + 2 {
                p.levels.pop_front();
            }
        }
    }
}

// ---- the window ------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Hit {
    Prev,
    Play,
    Next,
    Progress,
    Row(usize),
}

pub struct Music {
    lib: Vec<Track>,
    sel: usize,
    hits: Vec<(Rect, Hit)>,
    progress: Rect,
    /// Smoothed visualizer bars (0..=255 each).
    bars: [u8; BANDS],
    last_draw: u64,
    last_scan: u64,
}

impl Music {
    pub fn new() -> Music {
        Music {
            lib: library(),
            sel: 0,
            hits: Vec::new(),
            progress: Rect::new(0, 0, 0, 0),
            bars: [0; BANDS],
            last_draw: 0,
            last_scan: timer::now_ns(),
        }
    }

    fn current_index(&self, now: &Now) -> Option<usize> {
        let t = now.track.as_ref()?;
        self.lib.iter().position(|x| x.path == t.path)
    }

    fn play_index(&mut self, i: usize) {
        let Some(t) = self.lib.get(i).cloned() else {
            return;
        };
        let rest = self.lib[i + 1..]
            .iter()
            .filter(|x| x.system == t.system)
            .cloned()
            .collect();
        self.sel = i;
        play(t, rest);
    }

    fn step(&mut self, d: i32) {
        let now = now();
        let cur = self.current_index(&now).unwrap_or(self.sel) as i32;
        if self.lib.is_empty() {
            return;
        }
        // "Previous" restarts the track if we are more than 3 s in.
        if d < 0 && now.pos > 3 * RATE {
            seek(0);
            return;
        }
        let n = self.lib.len() as i32;
        self.play_index((cur + d).rem_euclid(n) as usize);
    }

    fn activate(&mut self, h: Hit, x: i32) {
        match h {
            Hit::Prev => self.step(-1),
            Hit::Next => self.step(1),
            Hit::Play => {
                if PLAYER.lock().track.is_some() {
                    toggle();
                } else {
                    self.play_index(self.sel);
                }
            }
            Hit::Progress => {
                if let Some(t) = now().track {
                    let r = self.progress;
                    let f = (x - r.x).clamp(0, r.w) as u64 * t.frames / r.w.max(1) as u64;
                    seek(f);
                }
            }
            Hit::Row(i) => {
                if self.sel == i || self.current_index(&now()) == Some(i) {
                    self.play_index(i);
                }
                self.sel = i;
            }
        }
    }
}

impl Drop for Music {
    fn drop(&mut self) {
        stop();
    }
}

fn mmss(frames: u64) -> String {
    let s = frames / RATE;
    format!("{}:{:02}", s / 60, s % 60)
}

/// A length: rounded up, so a half-second sound is 0:01, not 0:00.
fn length(frames: u64) -> String {
    mmss(frames.div_ceil(RATE) * RATE)
}

/// A right-pointing triangle `h` tall, its left edge at x.
fn triangle(p: &mut Painter, x: i32, y: i32, h: i32, c: crate::gfx::Color) {
    let half = h / 2;
    for k in 0..h {
        let w = (if k <= half { k } else { h - k }) * h * 7 / 8 / half.max(1);
        p.fill(x, y + k, w.max(1), 1, c);
    }
}

/// The same, pointing left, its right edge at xr.
fn triangle_left(p: &mut Painter, xr: i32, y: i32, h: i32, c: crate::gfx::Color) {
    let half = h / 2;
    for k in 0..h {
        let w = (if k <= half { k } else { h - k }) * h * 7 / 8 / half.max(1);
        p.fill(xr - w.max(1), y + k, w.max(1), 1, c);
    }
}

const ROW_H: i32 = 28;
const LIST_Y: i32 = 214;

impl WindowContents for Music {
    fn title(&self) -> &str {
        "Music"
    }

    fn menus(&self) -> &'static [&'static str] {
        &["File", "Edit", "Controls", "Window", "Help"]
    }

    fn draw(&mut self, p: &mut Painter, w: i32, h: i32, _focused: bool) {
        self.hits.clear();
        let now = now();
        let cur = self.current_index(&now);
        // Art: a straw-pink square with the note, the visualizer to its right.
        let art = Rect::new(20, 18, 96, 96);
        p.rrect_r(art, 14, STRAW, 255);
        icons::draw(
            p,
            art.x + 16,
            art.y + 16,
            64,
            Icon::Music,
            STRAW,
            0,
            INK,
            false,
        );
        let tx = art.x + art.w + 20;
        let (title, sub) = match &now.track {
            Some(t) => (
                t.title.clone(),
                String::from(if t.system { "System Sounds" } else { "~/Music" }),
            ),
            None => (
                String::from("Nothing playing"),
                String::from("Pick a tune below, or press Space"),
            ),
        };
        p.text_x(tx, 22, &title, ESPRESSO, 1, true);
        let note = if let Some(e) = &now.error {
            e.clone()
        } else if now.silent && now.playing {
            format!("{sub}   (no sound device: playing silently)")
        } else {
            sub
        };
        p.text_p(tx, 42, &note, COCOA, false);
        // Visualizer: 12 bars, rising fast and falling slowly.
        for (b, v) in self.bars.iter_mut().zip(now.levels) {
            *b = if v > *b {
                v
            } else {
                b.saturating_sub(24).max(v)
            };
        }
        let (vx, vy, vh) = (tx, 66, 48);
        let gap = 4;
        let bw = ((w - 20 - vx - gap * (BANDS as i32 - 1)) / BANDS as i32).clamp(4, 18);
        for (k, &b) in self.bars.iter().enumerate() {
            let x = vx + k as i32 * (bw + gap);
            p.rrect(x, vy, bw, vh, 3, OAT, 255);
            let bh = (b as i32 * vh / 255).max(if now.playing { 4 } else { 0 });
            if bh > 0 {
                let c = if k % 3 == 1 { STRAW } else { HONEY };
                p.rrect(x, vy + vh - bh, bw, bh, 3, c, 255);
            }
        }
        // Progress.
        let (len, pos) = now
            .track
            .as_ref()
            .map_or((0, 0), |t| (t.frames, now.pos.min(t.frames)));
        let pr = Rect::new(20, 132, w - 40, 6);
        self.progress = pr;
        p.rrect_r(pr, 3, SAND, 255);
        if let Some(fw) = (pos * pr.w as u64).checked_div(len) {
            let fw = fw as i32;
            p.rrect(pr.x, pr.y, fw.max(6), pr.h, 3, HONEY, 255);
            p.circle(pr.x + fw - 6, pr.y - 3, 12, ESPRESSO, 255);
        }
        self.hits
            .push((Rect::new(pr.x - 6, pr.y - 10, pr.w + 12, 26), Hit::Progress));
        p.text(pr.x, 146, &mmss(pos), COCOA);
        p.text_right(pr.x + pr.w, 146, &length(len), COCOA, 1, false);
        // Controls: previous, play / pause, next.
        let cx = w / 2;
        let cy = 182;
        p.circle(cx - 22, cy - 22, 44, ESPRESSO, 255);
        if now.playing {
            p.rrect(cx - 9, cy - 10, 6, 20, 2, MILK, 255);
            p.rrect(cx + 3, cy - 10, 6, 20, 2, MILK, 255);
        } else {
            triangle(p, cx - 6, cy - 11, 22, MILK);
        }
        self.hits
            .push((Rect::new(cx - 24, cy - 24, 48, 48), Hit::Play));
        for (dir, x0) in [(-1, cx - 74), (1, cx + 46)] {
            let y0 = cy - 8;
            if dir < 0 {
                p.fill(x0 + 2, y0, 3, 16, ESPRESSO);
                triangle_left(p, x0 + 16, y0, 16, ESPRESSO);
                triangle_left(p, x0 + 28, y0, 16, ESPRESSO);
            } else {
                triangle(p, x0, y0, 16, ESPRESSO);
                triangle(p, x0 + 12, y0, 16, ESPRESSO);
                p.fill(x0 + 23, y0, 3, 16, ESPRESSO);
            }
            self.hits.push((
                Rect::new(x0 - 6, cy - 18, 40, 36),
                if dir < 0 { Hit::Prev } else { Hit::Next },
            ));
        }
        // The library.
        p.fill(0, LIST_Y - 6, w, 1, SAND);
        let mut y = LIST_Y;
        let mut last_system = None;
        for (i, t) in self.lib.iter().enumerate() {
            if last_system != Some(t.system) {
                last_system = Some(t.system);
                p.text(
                    20,
                    y + 4,
                    if t.system { "SYSTEM SOUNDS" } else { "MUSIC" },
                    COCOA,
                );
                y += 24;
            }
            if y > h {
                break;
            }
            let r = Rect::new(10, y, w - 20, ROW_H - 2);
            if self.sel == i {
                p.rrect_r(r, R_ROW, STRAW, SEL_ROW_A);
            }
            if cur == Some(i) {
                let c = if now.playing { HONEY } else { LATTE };
                triangle(p, 22, y + 7, 12, c);
            }
            p.text_p(40, y + 5, &t.title, ESPRESSO, cur == Some(i));
            p.text_right(w - 22, y + 5, &length(t.frames), COCOA, 1, false);
            self.hits.push((r, Hit::Row(i)));
            y += ROW_H;
        }
        if self.lib.is_empty() {
            p.text_p(20, y, "No WAV files in ~/Music yet.", LATTE, false);
        }
    }

    fn pointer(&mut self, e: &PointerInput) -> bool {
        match e.kind {
            PointerKind::Down(_) | PointerKind::Drag => {
                let found = self
                    .hits
                    .iter()
                    .find(|(r, _)| hit(r, e.x, e.y))
                    .map(|x| x.1);
                match (e.kind, found) {
                    (PointerKind::Drag, Some(Hit::Progress)) | (PointerKind::Down(_), Some(_)) => {
                        self.activate(found.unwrap_or(Hit::Progress), e.x);
                        true
                    }
                    _ => false,
                }
            }
            PointerKind::Up(_) => false,
        }
    }

    fn key(&mut self, k: &KeyInput) -> bool {
        match k.key {
            Key::Char(' ') => self.activate(Hit::Play, 0),
            Key::Enter => self.play_index(self.sel),
            Key::Up => self.sel = self.sel.saturating_sub(1),
            Key::Down => self.sel = (self.sel + 1).min(self.lib.len().saturating_sub(1)),
            Key::Left => self.step(-1),
            Key::Right => self.step(1),
            _ => return false,
        }
        true
    }

    fn tick(&mut self, now_ns: u64) -> bool {
        // New files in ~/Music (the seed thread, a download): rescan every few seconds.
        if now_ns.saturating_sub(self.last_scan) > 3_000_000_000 {
            self.last_scan = now_ns;
            let lib = library();
            let changed = lib.len() != self.lib.len()
                || lib.iter().zip(&self.lib).any(|(a, b)| a.path != b.path);
            if changed {
                self.lib = lib;
                self.sel = self.sel.min(self.lib.len().saturating_sub(1));
                return true;
            }
        }
        // ~30 Hz while playing or while the bars fall back.
        let moving = PLAYER.lock().playing || self.bars.iter().any(|&b| b > 0);
        if moving && now_ns.saturating_sub(self.last_draw) >= 33_000_000 {
            self.last_draw = now_ns;
            return true;
        }
        false
    }
}
