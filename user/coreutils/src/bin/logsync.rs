//! logsync: save the SD card log now and say where it stands (Raspberry Pi; /dev/milklog).
//!
//! The kernel writes everything it says, and what the shell and the services print, into a file on the card's boot
//! partition (docs/raspberry-pi.md, "Reading the log"), once a second. This forces a write right now (the shell
//! prompt and the header included) and prints the log's status: the boot number, how far the text has got, bytes
//! waiting, write errors, lines dropped by the rate cap. Exit status 0: saved; 1: not saved (no log file on the card,
//! or the card refused the write); 2: this machine has no SD card log.
#![no_std]
#![no_main]
rt::main!(main);
fn main(_: &rt::Args) -> i32 {
    let fd = match rt::sys::open("/dev/milklog", rt::sys::O_WRONLY) {
        Ok(fd) => fd,
        Err(_) => {
            rt::eprintln!("logsync: this machine has no SD card log (there is no /dev/milklog)");
            return 2;
        }
    };
    let saved = rt::sys::write(fd, b"sync").is_ok();
    let _ = rt::sys::close(fd);
    // Read it from the start again (the write moved the file position).
    let Ok(fd) = rt::sys::open("/dev/milklog", rt::sys::O_RDONLY) else {
        return 2;
    };
    let mut buf = [0u8; 2048];
    let mut n = 0;
    while n < buf.len() {
        match rt::sys::read(fd, &mut buf[n..]) {
            Ok(0) | Err(_) => break,
            Ok(k) => n += k,
        }
    }
    let _ = rt::sys::close(fd);
    let _ = rt::sys::write_all(1, &buf[..n]);
    if saved {
        rt::println!("logsync: saved");
        0
    } else {
        rt::println!("logsync: NOT saved (see the status above: no usable MILKLOG.TXT, or the card refused the write)");
        1
    }
}
