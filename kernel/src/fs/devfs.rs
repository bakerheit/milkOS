//! devfs, mounted at `/dev`: `null`, `zero`, `random`, `urandom`, `console`.
//!
//! Device numbers match Linux (`mem` major 1, `console` 5:1) so userspace that checks
//! `st_rdev` sees familiar values. Drivers can add nodes later with [`add_device`].

use alloc::sync::Arc;
use core::fmt::Write;

use super::errno::{Errno, Result};
use super::node::{Inode, InodeRef, InodeType, Stat, makedev};
use super::synth::{Ids, SynthDir, SynthFs};
use crate::sync::SpinLock;
use crate::timer;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Null,
    Zero,
    Random,
    Console,
}

pub struct DevNode {
    dev: u64,
    ino: u64,
    rdev: u64,
    perm: u32,
    kind: Kind,
}

/// xorshift64* seeded from the counter. NOT cryptographic: fine for hash seeds and
/// games, not for keys.
static RNG: SpinLock<u64> = SpinLock::new(0);

fn next_random() -> u64 {
    let mut s = RNG.lock();
    if *s == 0 {
        // Mix the boot-relative counter so every boot differs a little.
        *s = timer::counter().wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    }
    let mut x = *s;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *s = x;
    x.wrapping_mul(0x2545_f491_4f6c_dd1d)
}

fn stir_random(data: &[u8]) {
    let mut s = RNG.lock();
    for chunk in data.chunks(8) {
        let mut w = [0u8; 8];
        w[..chunk.len()].copy_from_slice(chunk);
        *s = (*s ^ u64::from_ne_bytes(w)).rotate_left(17) | 1;
    }
}

impl Inode for DevNode {
    fn ino(&self) -> u64 {
        self.ino
    }
    fn dev(&self) -> u64 {
        self.dev
    }
    fn kind(&self) -> InodeType {
        InodeType::CharDev
    }
    fn stat(&self) -> Stat {
        let mut st = Stat::new(self.dev, self.ino, InodeType::CharDev, self.perm);
        st.rdev = self.rdev;
        st.blksize = 1024;
        st
    }

    fn read_at(&self, _off: u64, buf: &mut [u8]) -> Result<usize> {
        match self.kind {
            Kind::Null => Ok(0),
            Kind::Zero => {
                buf.fill(0);
                Ok(buf.len())
            }
            Kind::Random => {
                for chunk in buf.chunks_mut(8) {
                    let r = next_random().to_ne_bytes();
                    chunk.copy_from_slice(&r[..chunk.len()]);
                }
                Ok(buf.len())
            }
            // The tty line discipline (proc/tty.rs): blocks until a line is typed once
            // the scheduler runs; EAGAIN when empty before that.
            Kind::Console => crate::proc::tty::CONSOLE.read(buf),
        }
    }

    fn write_at(&self, _off: u64, buf: &[u8]) -> Result<usize> {
        match self.kind {
            Kind::Null | Kind::Zero => Ok(buf.len()),
            Kind::Random => {
                stir_random(buf);
                Ok(buf.len())
            }
            Kind::Console => {
                // Under the print lock, so a write isn't torn by another CPU's output.
                crate::sink::with_print_lock(|| {
                    let mut w = crate::console_writer();
                    for chunk in buf.utf8_chunks() {
                        let _ = w.write_str(chunk.valid());
                        if !chunk.invalid().is_empty() {
                            let _ = w.write_char('\u{fffd}');
                        }
                    }
                });
                Ok(buf.len())
            }
        }
    }

    fn truncate(&self, _len: u64) -> Result<()> {
        Ok(()) // `> /dev/null` opens with O_TRUNC
    }
}

static DEVFS: SpinLock<Option<Arc<SynthFs>>> = SpinLock::new(None);

fn node(ids: &Ids, kind: Kind, rdev: u64, perm: u32) -> InodeRef {
    Arc::new(DevNode {
        dev: ids.dev,
        ino: ids.next(),
        rdev,
        perm,
        kind,
    })
}

/// Build devfs and mount it on `/dev`.
pub fn init() -> Result<()> {
    let fs = SynthFs::new("devfs");
    let root: &Arc<SynthDir> = fs.root_dir();
    let ids = fs.ids();
    root.add("null", node(ids, Kind::Null, makedev(1, 3), 0o666));
    root.add("zero", node(ids, Kind::Zero, makedev(1, 5), 0o666));
    root.add("random", node(ids, Kind::Random, makedev(1, 8), 0o666));
    root.add("urandom", node(ids, Kind::Random, makedev(1, 9), 0o666));
    root.add("console", node(ids, Kind::Console, makedev(5, 1), 0o620));
    super::mount("/dev", fs.clone())?;
    *DEVFS.lock() = Some(fs);
    super::blockdev::publish_all();
    Ok(())
}

/// Publish a driver's device node as `/dev/<name>` (e.g. a future `vda` block device).
pub fn add_device(name: &str, inode: InodeRef) -> Result<()> {
    let fs = DEVFS.lock().clone().ok_or(Errno::ENODEV)?;
    fs.root_dir().add(name, inode);
    Ok(())
}

/// Inode numbering for drivers that build their own nodes for [`add_device`].
pub fn ids() -> Option<Arc<Ids>> {
    DEVFS.lock().as_ref().map(|f| f.ids().clone())
}
