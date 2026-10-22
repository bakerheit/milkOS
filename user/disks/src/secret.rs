//! Passphrases: asked for on the terminal without echo (or read as a line from a pipe), or
//! from a --passphrase-file, and wiped when done.

use alloc::string::String;
use alloc::vec::Vec;
use rt::{eprintln, sys};

const TCGETS: usize = 0x5401;
const TCSETS: usize = 0x5402;
const ECHO: u32 = 0o10;

fn tty(cmd: usize, t: &mut [u8; 60]) -> bool {
    // SAFETY: `t` is valid for the 36-byte termios TCGETS/TCSETS read and write.
    unsafe { sys::syscall(sys::nr::IOCTL, [0, cmd, t.as_mut_ptr() as usize, 0, 0, 0]) >= 0 }
}

/// Overwrite a secret before it is dropped.
pub fn wipe(s: &mut str) {
    // SAFETY: zero bytes are valid UTF-8.
    unsafe { s.as_bytes_mut().fill(0) };
}

/// Ask for a secret on the terminal without echoing it (or read a line from a pipe).
pub fn ask(prompt: &str) -> Option<String> {
    let mut t = [0u8; 60];
    let is_tty = tty(TCGETS, &mut t);
    let saved = t;
    if is_tty {
        let l = u32::from_le_bytes(t[12..16].try_into().unwrap()) & !ECHO;
        t[12..16].copy_from_slice(&l.to_le_bytes());
        tty(TCSETS, &mut t);
        let _ = sys::write_all(2, prompt.as_bytes());
    }
    let mut line = Vec::new();
    let mut b = [0u8; 1];
    while let Ok(1) = sys::read(0, &mut b) {
        if b[0] == b'\n' || b[0] == b'\r' {
            break;
        }
        line.push(b[0]);
    }
    if is_tty {
        let mut t = saved;
        tty(TCSETS, &mut t);
        let _ = sys::write_all(2, b"\n");
    }
    let s = String::from_utf8(line).ok()?;
    (!s.is_empty()).then_some(s)
}

/// A new passphrase, asked for twice.
pub fn new_passphrase(prompt: &str) -> Option<String> {
    let mut a = ask(prompt)?;
    let Some(mut b) = ask("Again: ") else {
        wipe(&mut a);
        return None;
    };
    let same = a == b;
    wipe(&mut b);
    if same {
        Some(a)
    } else {
        wipe(&mut a);
        eprintln!("disks: the passphrases differ");
        None
    }
}

/// A passphrase from `file` (its first line), else asked for (twice when `new`).
pub fn stream_passphrase(file: Option<&str>, new: bool) -> Option<String> {
    if let Some(f) = file {
        let mut d = match sys::read_file(f) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("disks: {}", filekit::words::say_read(f, e.0));
                return None;
            }
        };
        let s = core::str::from_utf8(&d)
            .ok()
            .and_then(|t| t.lines().next())
            .map(|l| String::from(l.trim_end()));
        d.fill(0);
        return s.filter(|s| !s.is_empty());
    }
    if new {
        new_passphrase("Passphrase for the stream (8 characters or more): ")
    } else {
        ask("Passphrase of the stream: ")
    }
}
