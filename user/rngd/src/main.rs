//! rngd: the virtio-rng driver as a user-space process (docs/drivers.md).
//!
//! Claims the device (the kernel driver lets go), sets up its one request queue, and feeds
//! the kernel CSPRNG through /dev/entropy: 64 bytes at start, then every 30 s. Each
//! request is one device-writable 64-byte buffer; the device's interrupt (the device fd)
//! says it is done. If rngd dies, the kernel resets the device and its own driver takes
//! over again; milkd restarts rngd.
//!
//!   rngd            run (milkd starts it: /System/Settings/milk/services/rngd.service)
//!   rngd -1         one request, print how many bytes went in, exit (tests)

#![no_std]
#![no_main]

extern crate alloc;

use rt::{eprintln, println, sys};
use udrv::{Device, Queue, Res, VIRTIO_RNG};

rt::main!(main);

const CHUNK: usize = 64;
const EVERY_MS: u64 = 30_000;

fn feed(dev: &Device, q: &mut Queue, buf: (usize, u64), out: i32) -> Res<usize> {
    let head = q.add(&[], &[(buf.1, CHUNK as u32)]).ok_or("queue full")?;
    if q.should_notify() {
        dev.notify(0)?;
    }
    loop {
        if let Some((h, len)) = q.pop_used() {
            q.free(h);
            if h != head {
                continue;
            }
            let n = (len as usize).min(CHUNK);
            // SAFETY: our DMA buffer; the device wrote `len` bytes and gave it back.
            let bytes = unsafe { core::slice::from_raw_parts_mut(buf.0 as *mut u8, n) };
            let r = sys::write_all(out, bytes);
            bytes.fill(0); // don't leave them lying around
            r.map_err(|e| alloc::format!("/dev/entropy: {e}"))?;
            return Ok(n);
        }
        dev.wait()?;
        dev.ack_and_arm();
    }
}

fn run(once: bool) -> Res<()> {
    let out = sys::open("/dev/entropy", sys::O_WRONLY | sys::O_CLOEXEC)
        .map_err(|e| alloc::format!("/dev/entropy: {e}"))?;
    let dev = Device::claim(VIRTIO_RNG)?;
    dev.init(0)?;
    let size = match dev.queue_max(0).min(8) {
        0 => return Err("the device has no request queue".into()),
        n => 1u16 << (15 - n.leading_zeros()),
    };
    let mut q = Queue::new(&dev, size)?;
    dev.setup_queue(0, &q)?;
    dev.driver_ok()?;
    let _ = sys::driver_arm(dev.fd);
    let buf = dev.dma(4096)?;
    let n = feed(&dev, &mut q, buf, out)?;
    println!(
        "rngd: virtio-rng ({}) is ours; fed {n} bytes to the kernel CSPRNG, then every {} s",
        dev.info(),
        EVERY_MS / 1000
    );
    if once {
        return Ok(());
    }
    loop {
        sys::sleep_ms(EVERY_MS);
        feed(&dev, &mut q, buf, out)?;
    }
}

fn main(args: &rt::Args) -> i32 {
    let once = args.get(1) == Some("-1");
    match run(once) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("rngd: {e}");
            1
        }
    }
}
