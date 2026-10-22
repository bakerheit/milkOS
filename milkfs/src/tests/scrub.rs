//! Scrub (`scripts/scrubtest.sh` without the scheduling and the rate limit, which are the
//! kernel's): a clean volume has no errors; a bit flipped in a live file's data and in a
//! snapshot-only file's are both found, named with their paths and every tree that has
//! the block; reading the damaged file fails its checksum; blocks freed while a scrub runs
//! are held back until it ends; a stopped scrub says so and lets go of them.

use std::cell::Cell;
use std::string::String;
use std::vec::Vec;

use super::harness::*;
use crate::errno::Errno;
use crate::format::*;
use crate::items::LIVE;
use crate::scrub::{self, ScrubRunner, Unlimited};

fn first_block(fs: &Fs, view: u64, path: &str) -> u64 {
    let ino = fs.resolve(view, path).unwrap();
    fs.st().exts(view, ino, 0, 1).unwrap()[0].block
}

fn flip(d: &MemDisk, block: u64) {
    let at = block as usize * BLOCK + 100;
    let b = d.peek(at, 1)[0];
    d.poke(at, &[b ^ 1]);
}

/// Primary volume: records the scrub as the last full one.
struct Primary;

impl ScrubRunner for Primary {
    fn records_last_scrub(&self) -> bool {
        true
    }
}

#[test]
fn finds_and_names_flipped_bits() {
    setup(80);
    let (d, fs) = Fs::fresh(128);
    fs.mkdir("/docs").unwrap();
    fs.write("/docs/b.bin", &noise(300_000)).unwrap();
    fs.write("/docs/old.bin", &noise(100_000)).unwrap();
    fs.write("/bulk.bin", &noise(4_000_000)).unwrap();
    fs.st().snap_create("s1").unwrap();
    fs.unlink("/docs/old.bin").unwrap();
    fs.commit().unwrap();
    let r = scrub::scrub(&fs, "/Volumes/Milk", &mut Primary).unwrap();
    assert!(r.walked.is_ok() && r.errors.is_empty(), "clean: {r:?}");
    assert!(r.done > 1000 && r.bytes == r.done * BLOCK as u64);
    assert!(fs.st().scrub.0 >= EPOCH as u64, "last full scrub recorded");
    assert!(fs.st().meta_dirty);
    let s1 = fs.st().snap_list()[0].generation;
    let (b, old) = (
        first_block(&fs, LIVE, "/docs/b.bin"),
        first_block(&fs, s1, "/docs/old.bin"),
    );
    drop(fs);
    flip(&d, b);
    flip(&d, old);
    let fs = Fs::mount(d.clone()).unwrap();
    let r = scrub::scrub(&fs, "/Volumes/Milk", &mut Unlimited).unwrap();
    let mut what: Vec<String> = r.errors.iter().map(|e| e.what.clone()).collect();
    what.sort();
    assert_eq!(
        what,
        [
            "data of /Volumes/Milk/docs/b.bin at offset 0 (live, snapshot s1)",
            "data of /Volumes/Milk/docs/old.bin at offset 0 (snapshot s1)",
        ]
    );
    assert_eq!(fs.st().store.held_count(), 0);
    // Reading the damaged file fails its checksum.
    take_log();
    assert_eq!(fs.read("/docs/b.bin").err(), Some(Errno::EIO));
    assert!(
        take_log()
            .iter()
            .any(|l| l.contains("checksum mismatch in block"))
    );
}

/// Deletes a file and writes another while the scrub reads (from its progress hook).
struct Busy<'a> {
    fs: &'a Fs,
    held: Cell<u64>,
    reads: u64,
}

impl ScrubRunner for Busy<'_> {
    fn read(&mut self, _n: usize, _done: u64) {
        self.reads += 1;
        if self.reads == 3 {
            self.fs.unlink("/a").unwrap();
            self.fs.commit().unwrap();
            self.fs.write("/c", b"").unwrap();
            self.fs.commit().unwrap(); // the frees of the first commit would be reused now
            self.held.set(self.fs.st().store.held_count());
            self.fs.write("/b2", &noise(400_000)).unwrap();
            self.fs.commit().unwrap();
            let c = self.fs.st().verify_space().unwrap();
            assert_eq!(
                (c.leaked, c.free_but_used),
                (0, 0),
                "held blocks count as free"
            );
        }
    }
}

#[test]
fn freed_blocks_are_held_while_it_reads() {
    setup(81);
    let (_d, fs) = Fs::fresh(32);
    fs.write("/a", &noise(400_000)).unwrap();
    fs.write("/b", &noise(400_000)).unwrap();
    fs.commit().unwrap();
    let mut run = Busy {
        fs: &fs,
        held: Cell::new(0),
        reads: 0,
    };
    let r = scrub::scrub(&fs, "/v", &mut run).unwrap();
    assert!(
        run.held.get() >= 98,
        "a's blocks were held ({})",
        run.held.get()
    );
    assert!(
        r.walked.is_ok() && r.errors.is_empty(),
        "nothing it read was overwritten: {r:?}"
    );
    assert_eq!(fs.st().store.held_count(), 0, "released at the end");
    fs.check_clean();
    fs.space_exact();
}

struct StopAfter(u64, u64);

impl ScrubRunner for StopAfter {
    fn stop(&self) -> bool {
        self.1 >= self.0
    }
    fn read(&mut self, _n: usize, _done: u64) {
        self.1 += 1;
    }
}

#[test]
fn a_stopped_scrub() {
    setup(82);
    let (_d, fs) = Fs::fresh(32);
    fs.write("/a", &noise(1_000_000)).unwrap();
    let r = scrub::scrub(&fs, "/v", &mut StopAfter(2, 0)).unwrap();
    assert_eq!(r.walked, Err(Errno::EINTR));
    assert!(
        r.done >= 2 && r.errors.is_empty(),
        "two reads, then it stopped: {r:?}"
    );
    assert_eq!(fs.st().store.held_count(), 0);
    assert_eq!(fs.st().scrub.0, 0, "not a full scrub");
}
