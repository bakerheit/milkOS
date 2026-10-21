//! play: sound out of /dev/audio (S16_LE, 48 kHz, stereo).
//!   play tone FREQ SECS [VOLUME]      sine tone (VOLUME 0..1, default 0.3), soft edges
//!   play chime                        the boot chime (two soft sine notes)
//!   play FILE.wav                     16-bit PCM WAV, mono or stereo, any rate
//!   play system notify|error|chime    a kernel system sound (as the desktop plays them)
//! Waits until the sound has been played (SNDCTL_DSP_SYNC) unless -n is given first.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use rt::{eprintln, sys};

rt::main!(main);

const RATE: f64 = 48_000.0;
const SNDCTL_DSP_SYNC: u32 = 0x5001;
const MILK_AUDIO_SYSTEM: u32 = 0x4d01;
const PI: f64 = core::f64::consts::PI;

/// sin(x) without libm: reduce to [-pi, pi], then an odd Taylor polynomial (error < 1e-7).
fn sin(x: f64) -> f64 {
    let two_pi = 2.0 * PI;
    let mut x = x - two_pi * ((x / two_pi) as i64 as f64);
    if x > PI {
        x -= two_pi;
    } else if x < -PI {
        x += two_pi;
    }
    // Fold into [-pi/2, pi/2] for a faster-converging series.
    if x > PI / 2.0 {
        x = PI - x;
    } else if x < -PI / 2.0 {
        x = -PI - x;
    }
    let x2 = x * x;
    let mut term = x;
    let mut sum = x;
    for k in 1..8 {
        term *= -x2 / ((2 * k) as f64 * (2 * k + 1) as f64);
        sum += term;
    }
    sum
}

/// Interleaved stereo i16 from a mono f64 signal in -1..1.
fn to_pcm(mono: &[f64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(mono.len() * 4);
    for &v in mono {
        let s = (v.clamp(-1.0, 1.0) * 32767.0) as i16;
        out.extend_from_slice(&s.to_le_bytes());
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// A sine tone with 10 ms raised edges (no clicks).
fn tone(freq: f64, secs: f64, vol: f64) -> Vec<f64> {
    let n = (secs * RATE) as usize;
    let edge = (0.010 * RATE) as usize;
    (0..n)
        .map(|i| {
            let e = if i < edge {
                i as f64 / edge as f64
            } else if n - i < edge {
                (n - i) as f64 / edge as f64
            } else {
                1.0
            };
            vol * e * sin(2.0 * PI * freq * i as f64 / RATE)
        })
        .collect()
}

/// A soft bell-ish note: fundamental + a quiet octave, quick attack, exponential decay.
fn note(out: &mut [f64], start: f64, freq: f64, len: f64, vol: f64) {
    let s0 = (start * RATE) as usize;
    let n = (len * RATE) as usize;
    let attack = (0.012 * RATE) as usize;
    let decay = 1.0 - 1.0 / (0.18 * RATE); // ~180 ms time constant
    let mut env_d = 1.0;
    for i in 0..n {
        let Some(o) = out.get_mut(s0 + i) else { break };
        let a = if i < attack {
            i as f64 / attack as f64
        } else {
            1.0
        };
        let fade = if n - i < attack {
            (n - i) as f64 / attack as f64
        } else {
            1.0
        };
        let t = i as f64 / RATE;
        let v = sin(2.0 * PI * freq * t) + 0.25 * sin(4.0 * PI * freq * t);
        *o += vol * a * fade * env_d * v / 1.25;
        env_d *= decay;
    }
}

/// The boot chime: G5 then C6, overlapping, about 0.9 s.
fn chime() -> Vec<f64> {
    let mut out = alloc::vec![0.0; (0.95 * RATE) as usize];
    note(&mut out, 0.0, 783.99, 0.6, 0.22);
    note(&mut out, 0.16, 1046.50, 0.78, 0.22);
    out
}

fn le16(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(o)?, *b.get(o + 1)?]))
}
fn le32(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

/// 16-bit PCM WAV -> interleaved stereo 48 kHz bytes. Err: a message.
fn wav(b: &[u8]) -> Result<(Vec<u8>, u16, u32), &'static str> {
    if b.get(0..4) != Some(b"RIFF") || b.get(8..12) != Some(b"WAVE") {
        return Err("not a RIFF/WAVE file");
    }
    let (mut ch, mut rate, mut bits, mut fmt) = (0u16, 0u32, 0u16, 0u16);
    let mut data: &[u8] = &[];
    let mut k = 12;
    while k + 8 <= b.len() {
        let len = le32(b, k + 4).ok_or("truncated")? as usize;
        let body = &b[k + 8..(k + 8 + len).min(b.len())];
        match &b[k..k + 4] {
            b"fmt " => {
                fmt = le16(body, 0).ok_or("bad fmt")?;
                ch = le16(body, 2).ok_or("bad fmt")?;
                rate = le32(body, 4).ok_or("bad fmt")?;
                bits = le16(body, 14).ok_or("bad fmt")?;
            }
            b"data" => data = body,
            _ => {}
        }
        k += 8 + len + (len & 1);
    }
    if !(fmt == 1 || fmt == 0xfffe) || bits != 16 || !(1..=2).contains(&ch) || rate == 0 {
        return Err("only 16-bit PCM, mono or stereo");
    }
    let c = ch as usize;
    let frames = data.len() / (2 * c);
    if frames == 0 {
        return Ok((Vec::new(), ch, rate));
    }
    let at = |f: usize, k: usize| -> i32 {
        let o = (f.min(frames - 1) * c + k.min(c - 1)) * 2;
        i16::from_le_bytes([data[o], data[o + 1]]) as i32
    };
    let out_frames = (frames as u64 * 48_000 / rate as u64) as usize;
    let mut out = Vec::with_capacity(out_frames * 4);
    for i in 0..out_frames {
        let pos = i as u64 * rate as u64;
        let f = (pos / 48_000) as usize;
        let frac = (pos % 48_000) as i32;
        for k in 0..2 {
            let (a, b2) = (at(f, k), at(f + 1, k));
            out.extend_from_slice(&((a + (b2 - a) * frac / 48_000) as i16).to_le_bytes());
        }
    }
    Ok((out, ch, rate))
}

fn usage() -> i32 {
    eprintln!(
        "usage: play [-n] tone FREQ SECS [VOLUME] | chime | FILE.wav | system notify|error|chime"
    );
    2
}

fn main(args: &rt::Args) -> i32 {
    let mut a: Vec<&str> = args.argv.iter().skip(1).copied().collect();
    let wait = !(a.first() == Some(&"-n"));
    if !wait {
        a.remove(0);
    }
    let fd = match sys::open("/dev/audio", sys::O_WRONLY | sys::O_CLOEXEC) {
        Ok(fd) => fd,
        Err(e) => {
            eprintln!("play: /dev/audio: {e} (no sound device?)");
            return 1;
        }
    };
    let pcm = match a.as_slice() {
        ["tone", f, s, rest @ ..] => {
            let (Ok(f), Ok(s)) = (f.parse::<f64>(), s.parse::<f64>()) else {
                return usage();
            };
            let vol = rest
                .first()
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or(0.3);
            if !(1.0..=20_000.0).contains(&f) || !(0.0..=60.0).contains(&s) {
                eprintln!("play: tone: 1..20000 Hz, up to 60 s");
                return 2;
            }
            to_pcm(&tone(f, s, vol.clamp(0.0, 1.0)))
        }
        ["chime"] => to_pcm(&chime()),
        ["system", which] => {
            let n = match *which {
                "notify" => 0,
                "error" => 1,
                "chime" => 2,
                _ => return usage(),
            };
            if let Err(e) = sys::ioctl(fd, MILK_AUDIO_SYSTEM, n) {
                eprintln!("play: system sound: {e}");
                return 1;
            }
            return 0;
        }
        [file] if !file.starts_with('-') => {
            let bytes = match sys::read_file(file) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("play: {file}: {e}");
                    return 1;
                }
            };
            match wav(&bytes) {
                Ok((p, _, _)) => p,
                Err(m) => {
                    eprintln!("play: {file}: {m}");
                    return 1;
                }
            }
        }
        _ => return usage(),
    };
    if let Err(e) = sys::write_all(fd, &pcm) {
        eprintln!("play: write: {e}");
        return 1;
    }
    if wait {
        let _ = sys::ioctl(fd, SNDCTL_DSP_SYNC, 0);
    }
    let _ = sys::close(fd);
    0
}
