//! PID 1. Prints the banner, runs `hello` once, then keeps a shell on the console
//! (restarting it when it exits) and reaps every orphan that gets reparented to it.

#![no_std]
#![no_main]

use rt::{println, sys};

rt::main!(main);

const ENV: &[&str] = &["PATH=/bin", "HOME=/home/milk", "TERM=dumb"];

fn main(_args: &rt::Args) -> i32 {
    let u = sys::uname().unwrap_or_default();
    println!();
    println!(
        "  milkOS init (pid {}): {} {} {}",
        sys::getpid(),
        u[0],
        u[2],
        u[4]
    );
    println!("  user space is running. Starting /bin/hello, then the shell.");
    println!();

    // Installed packages: recreate their /bin links (/bin is in RAM, /Applications is not).
    if let Ok(pid) = sys::spawn("/bin/milk", &["milk", "relink", "-q"], ENV) {
        wait_for(pid);
    }
    // The boot chime, if there is a sound device (it plays while we go on).
    if let Ok(fd) = sys::open("/dev/audio", sys::O_WRONLY | sys::O_CLOEXEC) {
        let _ = sys::close(fd);
        if let Ok(pid) = sys::spawn("/bin/play", &["play", "-n", "chime"], ENV) {
            quiet(pid);
        }
    }
    // The web server for :80 (the kernel's own httpd is off by default).
    if sys::read_file("/srv/www/index.html").is_ok() {
        match sys::spawn("/bin/httpd", &["httpd", "-q", "/srv/www"], ENV) {
            Ok(pid) => {
                quiet(pid);
                println!("  httpd (pid {pid}) serving /srv/www on :80");
            }
            Err(e) => println!("init: cannot start /bin/httpd: {e}"),
        }
    }

    if let Ok(pid) = sys::spawn("/bin/hello", &["hello", "from", "init"], ENV) {
        wait_for(pid);
    }
    loop {
        let sh = match sys::spawn("/bin/msh", &["msh"], ENV) {
            Ok(p) => p,
            Err(e) => {
                println!("init: cannot start /bin/msh: {e}");
                sys::sleep_ms(1000);
                continue;
            }
        };
        let st = wait_for(sh);
        println!(
            "init: msh exited ({}), starting a new one",
            sys::describe_status(st)
        );
    }
}

/// Our own background helpers (chime, httpd): reaped without a message when they exit 0.
static mut QUIET: [u32; 4] = [0; 4];

fn quiet(pid: u32) {
    // SAFETY: init is single-threaded.
    unsafe {
        let q = &mut *core::ptr::addr_of_mut!(QUIET);
        if let Some(slot) = q.iter_mut().find(|p| **p == 0) {
            *slot = pid;
        }
    }
}

fn is_quiet(pid: u32) -> bool {
    // SAFETY: as above.
    unsafe { (*core::ptr::addr_of!(QUIET)).contains(&pid) }
}

/// Reap children until `pid` is among them; return its status.
fn wait_for(pid: u32) -> i32 {
    loop {
        match sys::wait4(-1, 0) {
            Ok((p, st)) if p == pid => return st,
            Ok((p, 0)) if is_quiet(p) => {}
            Ok((p, st)) => println!("init: reaped pid {p} ({})", sys::describe_status(st)),
            Err(e) => {
                println!("init: wait4: {e:?}");
                sys::sleep_ms(100);
            }
        }
    }
}
