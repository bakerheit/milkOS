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

    // The web server for :80 (the kernel's own httpd is off by default).
    if sys::read_file("/srv/www/index.html").is_ok() {
        match sys::spawn("/bin/httpd", &["httpd", "-q", "/srv/www"], ENV) {
            Ok(pid) => println!("  httpd (pid {pid}) serving /srv/www on :80"),
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

/// Reap children until `pid` is among them; return its status.
fn wait_for(pid: u32) -> i32 {
    loop {
        match sys::wait4(-1, 0) {
            Ok((p, st)) if p == pid => return st,
            Ok((p, st)) => println!("init: reaped pid {p} ({})", sys::describe_status(st)),
            Err(e) => {
                println!("init: wait4: {e:?}");
                sys::sleep_ms(100);
            }
        }
    }
}
