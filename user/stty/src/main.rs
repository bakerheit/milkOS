//! stty: show or change the terminal's mode and size (TCGETS / TCSETS / TIOCGWINSZ).
//!   stty            mode and size
//!   stty size       "rows cols" (what scripts expect)
//!   stty raw        no line editing, no echo, no ^C (programs read every key)
//!   stty sane       back to cooked mode with echo
//!   stty -echo / echo

#![no_std]
#![no_main]

use rt::{println, sys};

rt::main!(main);

const IOCTL: usize = 29;
const TCGETS: usize = 0x5401;
const TCSETS: usize = 0x5402;
const TIOCGWINSZ: usize = 0x5413;
const ISIG: u32 = 0o1;
const ICANON: u32 = 0o2;
const ECHO: u32 = 0o10;
const ICRNL: u32 = 0o400;

fn ioctl(cmd: usize, buf: &mut [u8]) -> isize {
    // SAFETY: buf is valid for the size the ioctl reads or writes (36 / 8 bytes).
    unsafe { sys::syscall(IOCTL, [0, cmd, buf.as_mut_ptr() as usize, 0, 0, 0]) }
}

fn word(t: &[u8], k: usize) -> u32 {
    u32::from_le_bytes([t[k], t[k + 1], t[k + 2], t[k + 3]])
}

fn set_word(t: &mut [u8], k: usize, v: u32) {
    t[k..k + 4].copy_from_slice(&v.to_le_bytes());
}

fn main(args: &rt::Args) -> i32 {
    let mut t = [0u8; 60];
    if ioctl(TCGETS, &mut t) < 0 {
        println!("stty: standard input is not a terminal");
        return 1;
    }
    let mut ws = [0u8; 8];
    let have_ws = ioctl(TIOCGWINSZ, &mut ws) >= 0;
    let rows = u16::from_le_bytes([ws[0], ws[1]]);
    let cols = u16::from_le_bytes([ws[2], ws[3]]);
    let (iflag, lflag) = (word(&t, 0), word(&t, 12));
    match args.get(1) {
        None => {
            let mode = if lflag & ICANON != 0 { "cooked" } else { "raw" };
            let echo = if lflag & ECHO != 0 { "echo" } else { "-echo" };
            if have_ws {
                println!("{mode} {echo}, {cols} columns x {rows} rows");
            } else {
                println!("{mode} {echo}");
            }
            0
        }
        Some("size") => {
            println!("{rows} {cols}");
            0
        }
        Some(cmd) => {
            let (i, l) = match cmd {
                "raw" => (iflag & !ICRNL, lflag & !(ICANON | ECHO | ISIG)),
                "sane" | "cooked" => (iflag | ICRNL, lflag | ICANON | ECHO | ISIG),
                "-echo" => (iflag, lflag & !ECHO),
                "echo" => (iflag, lflag | ECHO),
                _ => {
                    println!("stty: unknown setting '{cmd}' (try raw, sane, echo, -echo, size)");
                    return 1;
                }
            };
            set_word(&mut t, 0, i);
            set_word(&mut t, 12, l);
            if ioctl(TCSETS, &mut t) < 0 {
                println!("stty: TCSETS failed");
                return 1;
            }
            0
        }
    }
}
