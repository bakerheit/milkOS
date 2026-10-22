//! Encryption at rest (`scripts/enctest.sh`, the parts below the shell): an encrypted
//! volume holds no plaintext (contents or names); without the key it doesn't mount; a
//! wrong passphrase is refused, the right one and the recovery key open it; changing the
//! passphrase rewraps the key only (the old one stops working, the new one and the
//! recovery key do); a tampered superblock refuses to mount; locking drops the key and
//! refuses writes; check and scrub are clean, and a damaged keyring is found.

use std::string::String;
use std::sync::Arc;
use std::vec::Vec;

use super::harness::*;
use crate::blockdev::BlockRef;
use crate::crypt::{self, VolumeKey};
use crate::errno::{Errno, Result};
use crate::format::*;
use crate::scrub::{self, Unlimited};
use crate::volume::{self, State};

pub const P1: &str = "not-a-real-passphrase";
pub const P2: &str = "another-fake-test-passphrase";
pub const CANARY: &[u8] = b"MILKFS-CANARY-PLAINTEXT-7f3a9c";

/// The volume key from `secret`, as the kernel's `unlock_key` gets it (the keyring read
/// straight from the disk).
pub fn unlock(dev: &BlockRef, secret: &str) -> Result<Arc<VolumeKey>> {
    let sb = volume::probe(dev).ok_or(Errno::EINVAL)?;
    let per = (BLOCK / dev.block_size()) as u64;
    let mut b = [0u8; BLOCK];
    dev.read_blocks(sb.keyring.block * per, &mut b)?;
    if sb.keyring.is_none() || xxh64(&b) != sb.keyring.csum {
        return Err(Errno::EIO);
    }
    let ring = Keyring::decode(&b).ok_or(Errno::EIO)?;
    if ring.uuid != sb.uuid {
        return Err(Errno::EIO);
    }
    crypt::unlock(&ring, secret).map(|(k, _)| Arc::new(k))
}

pub fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// An encrypted volume with a recovery key, a file with the canary written into it.
fn encrypted() -> (Arc<MemDisk>, String) {
    let d = MemDisk::new(32 << 20);
    let dev: BlockRef = d.clone();
    let rec = volume::mkfs_encrypted(&dev, "Milk", P1, true)
        .unwrap()
        .unwrap();
    let key = unlock(&dev, P1).unwrap();
    let fs = Fs::mount_key(dev, key).unwrap();
    fs.mkdir("/docs").unwrap();
    let mut text = CANARY.to_vec();
    text.extend_from_slice(b" and some text around it\n");
    fs.write("/docs/canary-file.txt", &text).unwrap();
    fs.write("/docs/bulk", &[CANARY; 400].concat()).unwrap();
    fs.commit().unwrap();
    (d, rec)
}

#[test]
fn no_plaintext_keys_and_the_recovery_key() {
    setup(90);
    let (d, rec) = encrypted();
    let img = d.image();
    assert!(
        !contains(&img, CANARY),
        "the canary is on the disk in plaintext"
    );
    assert!(
        !contains(&img, b"canary-file"),
        "a file name is on the disk in plaintext"
    );
    let dev: BlockRef = d.clone();
    let sb = volume::probe(&dev).unwrap();
    assert!(needs_key(sb.flags) && sb.version == CRYPT_VERSION);
    assert_eq!(
        State::open(dev.clone()).err(),
        Some(Errno::EACCES),
        "no key, no mount"
    );
    assert_eq!(
        unlock(&dev, "wrong-fake-passphrase").err(),
        Some(Errno::EACCES)
    );
    assert!(
        rec.len() == 39 && rec.split('-').count() == 8,
        "recovery key shown as {rec}"
    );
    for secret in [P1, rec.as_str(), &rec.to_lowercase().replace('-', " ")] {
        let fs = Fs::mount_key(dev.clone(), unlock(&dev, secret).unwrap()).unwrap();
        assert!(
            fs.read("/docs/canary-file.txt")
                .unwrap()
                .starts_with(CANARY)
        );
        fs.check_clean();
    }
    // The wrong key (another volume's) reads nothing.
    let other = Arc::new(VolumeKey::from_master(&[7; 32]));
    assert_eq!(
        State::open_with(dev, Some(other)).err(),
        Some(Errno::EIO),
        "superblock MAC"
    );
}

#[test]
fn change_the_passphrase_rewraps_only() {
    setup(91);
    let (d, rec) = encrypted();
    let dev: BlockRef = d.clone();
    let fs = Fs::mount_key(dev.clone(), unlock(&dev, P1).unwrap()).unwrap();
    let before = d.image();
    assert_eq!(
        fs.st().rewrap("wrong-fake-passphrase", P2),
        Err(Errno::EACCES)
    );
    fs.st().rewrap(P1, P2).unwrap();
    fs.write("/docs/inside.txt", b"written after passwd")
        .unwrap();
    fs.commit().unwrap();
    drop(fs);
    assert_eq!(
        unlock(&dev, P1).err(),
        Some(Errno::EACCES),
        "the old one no longer opens it"
    );
    let fs = Fs::mount_key(dev.clone(), unlock(&dev, P2).unwrap()).unwrap();
    assert_eq!(
        fs.read("/docs/inside.txt").unwrap(),
        b"written after passwd"
    );
    fs.check_clean();
    drop(fs);
    let fs = Fs::mount_key(dev.clone(), unlock(&dev, &rec).unwrap()).unwrap();
    assert!(
        fs.read("/docs/canary-file.txt")
            .unwrap()
            .starts_with(CANARY)
    );
    drop(fs);
    // No data block was rewritten by the rewrap: the canary file's blocks are as they were.
    let after = d.image();
    let fs = Fs::mount_key(dev.clone(), unlock(&dev, P2).unwrap()).unwrap();
    let ino = fs.resolve(0, "/docs/bulk").unwrap();
    for e in fs.st().exts(0, ino, 0, u64::MAX).unwrap() {
        for b in e.block..e.block + e.len() {
            let r = b as usize * BLOCK..(b as usize + 1) * BLOCK;
            assert!(before[r.clone()] == after[r], "block {b} changed");
        }
    }
    assert!(!contains(&after, CANARY) && !contains(&after, b"written after passwd"));
}

#[test]
fn a_tampered_superblock_refuses_to_mount() {
    setup(92);
    let (d, _) = encrypted();
    let dev: BlockRef = d.clone();
    let key = unlock(&dev, P1).unwrap();
    let sb = volume::probe(&dev).unwrap();
    let slot = (sb.generation % 2) as usize * BLOCK;
    // A field changed and the checksum fixed up: it decodes fine, but the MAC (still the
    // old one) says no.
    let mut sb2 = sb.clone();
    sb2.next_ino += 1;
    d.poke(slot, &sb2.encode());
    assert!(Superblock::decode(&d.peek(slot, BLOCK).try_into().unwrap()).is_some());
    take_log();
    assert_eq!(State::open_with(dev, Some(key)).err(), Some(Errno::EIO));
    assert!(
        take_log()
            .iter()
            .any(|l| l.contains("superblock MAC mismatch"))
    );
}

#[test]
fn lock_drops_the_key_and_refuses_writes() {
    setup(93);
    let (d, _) = encrypted();
    let dev: BlockRef = d.clone();
    let fs = Fs::mount_key(dev.clone(), unlock(&dev, P1).unwrap()).unwrap();
    fs.write("/docs/last.txt", b"just before the lock").unwrap();
    fs.commit().unwrap();
    assert!(fs.st().forget_key(), "the key was wiped right there");
    assert_eq!(fs.write("/docs/x", b"x").err(), Some(Errno::EIO));
    assert!(fs.st().store.crypt.is_none());
    drop(fs);
    let fs = Fs::mount_key(dev.clone(), unlock(&dev, P1).unwrap()).unwrap();
    assert_eq!(fs.read("/docs/last.txt").unwrap(), b"just before the lock");
}

#[test]
fn scrub_and_check_an_encrypted_volume() {
    setup(94);
    let (d, _) = encrypted();
    let dev: BlockRef = d.clone();
    let fs = Fs::mount_key(dev.clone(), unlock(&dev, P1).unwrap()).unwrap();
    fs.check_clean();
    fs.space_exact();
    let r = scrub::scrub(&fs, "/Volumes/Milk", &mut Unlimited).unwrap();
    assert!(r.walked.is_ok() && r.errors.is_empty(), "{r:?}");
    // A damaged keyring is found (it is plaintext, checked as such).
    let kr = fs.st().keyring.block as usize;
    drop(fs);
    let key = unlock(&dev, P1).unwrap();
    d.poke(kr * BLOCK + 200, &[0xff; 4]);
    let fs = Fs::mount_key(dev.clone(), key).unwrap();
    let r = scrub::scrub(&fs, "/Volumes/Milk", &mut Unlimited).unwrap();
    let what: Vec<&str> = r.errors.iter().map(|e| e.what.as_str()).collect();
    assert_eq!(what, ["keyring (the wrapped keys)"]);
    assert_eq!(
        unlock(&dev, P1).err(),
        Some(Errno::EIO),
        "and nothing unlocks it now"
    );
}
