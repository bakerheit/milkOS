//! Minimal WAV (RIFF) reader: PCM 16-bit, 1 or 2 channels, any rate, converted to the
//! mixer's format (S16 stereo 48 kHz, interleaved).

use alloc::vec::Vec;

use crate::virtio::sound::RATE;

fn le16(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(o)?, *b.get(o + 1)?]))
}
fn le32(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

/// Parse a WAV file into interleaved stereo i16 at 48 kHz. None if it is not 16-bit PCM.
pub fn decode(b: &[u8]) -> Option<Vec<i16>> {
    if b.get(0..4)? != b"RIFF" || b.get(8..12)? != b"WAVE" {
        return None;
    }
    let (mut channels, mut rate, mut bits, mut fmt) = (0u16, 0u32, 0u16, 0u16);
    let mut data: Option<&[u8]> = None;
    let mut k = 12;
    while k + 8 <= b.len() {
        let id = &b[k..k + 4];
        let len = le32(b, k + 4)? as usize;
        let body = b.get(k + 8..(k + 8 + len).min(b.len()))?;
        match id {
            b"fmt " => {
                fmt = le16(body, 0)?;
                channels = le16(body, 2)?;
                rate = le32(body, 4)?;
                bits = le16(body, 14)?;
            }
            b"data" => data = Some(body),
            _ => {}
        }
        k += 8 + len + (len & 1);
    }
    // 1 = PCM, 0xFFFE = WAVE_FORMAT_EXTENSIBLE (assume PCM inside).
    if !(fmt == 1 || fmt == 0xfffe) || bits != 16 || !(1..=2).contains(&channels) || rate == 0 {
        return None;
    }
    let data = data?;
    let ch = channels as usize;
    let frames = data.len() / (2 * ch);
    let sample = |f: usize, c: usize| -> i32 {
        let o = (f * ch + c.min(ch - 1)) * 2;
        i16::from_le_bytes([data[o], data[o + 1]]) as i32
    };
    // Linear resampling to 48 kHz (identity when the rate already matches).
    let out_frames = (frames as u64 * RATE as u64 / rate as u64) as usize;
    let mut out = Vec::with_capacity(out_frames * 2);
    for i in 0..out_frames {
        let pos = i as u64 * rate as u64; // in units of 1/RATE input frames
        let f = (pos / RATE as u64) as usize;
        let frac = (pos % RATE as u64) as i32;
        for c in 0..2 {
            let a = sample(f.min(frames - 1), c);
            let b2 = sample((f + 1).min(frames - 1), c);
            out.push((a + (b2 - a) * frac / RATE as i32) as i16);
        }
    }
    Some(out)
}
