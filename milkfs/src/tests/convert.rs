//! Encrypting and decrypting in place, with crashes (`scripts/converttest.sh`): a volume
//! the crash writer filled (files, reflink copies, deduped copies, snapshots, a promoted
//! clone) is converted while the power goes at random moments. After every crash the
//! volume mounts, checks clean, and every tree (live, every snapshot and clone) is
//! exactly the base's; the crash check passes; the conversion resumes and finishes, the
//! trees still the base's, and (encrypting) none of the workload's plaintext is left on
//! the disk. Then the same from encrypted back to plain.

use std::collections::BTreeMap;
use std::string::String;
use std::sync::Arc;
use std::vec::Vec;

use super::crypt::{P1, contains};
use super::harness::*;
use super::workload;
use crate::blockdev::BlockRef;
use crate::convert::Step;
use crate::crypt::{self, VolumeKey};
use crate::format::*;
use crate::items::{LIVE, clone_view};
use crate::volume;

/// Every tree's listing (live, snapshots and clones by name).
fn trees(fs: &Fs) -> BTreeMap<String, Vec<String>> {
    let mut m = BTreeMap::new();
    m.insert(String::from("live"), fs.tree(LIVE).unwrap());
    let snaps = fs.st().snap_list();
    let clones = fs.st().clone_list();
    for s in snaps {
        m.insert(s.name.clone(), fs.tree(s.generation).unwrap());
    }
    for c in clones {
        m.insert(
            std::format!("clone {}", c.name),
            fs.tree(clone_view(c.id)).unwrap(),
        );
    }
    m
}

/// The new volume key's master bytes, its keyring (a passphrase slot and a recovery slot)
/// and the recovery key: made once (scrypt is slow), used for every round.
struct Keys {
    vk: [u8; 32],
    ring: Keyring,
    recovery: String,
}

fn keys(uuid: [u8; 16]) -> Keys {
    let mut vk = [0u8; 32];
    crate::platform::fill_random(&mut vk);
    let key = VolumeKey::from_master(&vk);
    let mut rk = [0u8; 16];
    crate::platform::fill_random(&mut rk);
    let ring = Keyring {
        uuid,
        slots: std::vec![
            crypt::wrap(&uuid, &key, SLOT_PASSPHRASE, P1.as_bytes()).unwrap(),
            crypt::wrap(&uuid, &key, SLOT_RECOVERY, &rk).unwrap(),
        ],
    };
    Keys {
        vk,
        ring,
        recovery: crypt::format_recovery(&rk),
    }
}

impl Keys {
    fn key(&self) -> Arc<VolumeKey> {
        Arc::new(VolumeKey::from_master(&self.vk))
    }
}

/// Mount a volume that may need the key (encrypted, or being converted).
fn mount(dev: BlockRef, k: &Keys) -> Fs {
    let sb = volume::probe(&dev).unwrap();
    if needs_key(sb.flags) {
        Fs::mount_key(dev, k.key()).unwrap()
    } else {
        Fs::mount(dev).unwrap()
    }
}

/// Run the conversion (as the kernel's convert thread does, 64 blocks a step) until it is
/// done or the disk dies. True if it finished.
fn convert(fs: &Fs, disk: Option<&CrashDisk>) -> bool {
    for _ in 0..1_000_000 {
        match fs.st().convert_step(64) {
            Ok(Step::Done) => return true,
            Ok(Step::More) => {}
            Ok(Step::NeedSpace(n)) => panic!("needs {n} more free blocks"),
            Err(e) => {
                assert!(disk.is_some_and(|d| d.dead()), "conversion failed: {e}");
                return false;
            }
        }
        if disk.is_some_and(|d| d.dead()) {
            return false;
        }
    }
    panic!("the conversion never finished");
}

/// What a crash round must show (see the module docs). `encrypt`: the direction.
fn check_round(
    what: &str,
    left: Vec<u8>,
    k: &Keys,
    base: &BTreeMap<String, Vec<String>>,
    encrypt: bool,
) -> bool {
    let fs = mount(MemDisk::from_image(left.clone()), k);
    fs.check_clean();
    assert_eq!(&trees(&fs), base, "{what}: every tree is the base's");
    let mid = {
        let st = fs.st();
        st.converting() && st.conv.as_ref().is_some_and(|c| c.state.done > 0)
    };
    drop(fs);
    // Resume (on a copy) and finish.
    let d = MemDisk::from_image(left.clone());
    let fs = mount(d.clone(), k);
    if fs.st().converting() {
        assert!(convert(&fs, None), "{what}: resumed and finished");
    } else if encrypt != fs.st().store.encrypted() {
        // The crash came before the conversion's first commit: start it again.
        start(&fs, k, encrypt);
        assert!(convert(&fs, None));
    }
    assert_eq!(fs.st().store.encrypted(), encrypt, "{what}");
    assert!(!fs.st().converting());
    fs.check_clean();
    assert_eq!(&trees(&fs), base, "{what}: still the same trees");
    drop(fs);
    let img = d.image();
    if encrypt {
        assert!(
            !contains(&img, b"milkcrash i="),
            "{what}: plaintext left on the disk"
        );
        assert!(
            volume::State::open(d.clone()).is_err(),
            "{what}: no key, no mount"
        );
    } else {
        let fs = Fs::mount(d).unwrap(); // plain again: no key needed
        fs.check_clean();
    }
    // The crash check on what the crash left.
    let fs = mount(MemDisk::from_image(left), k);
    let v = workload::crash_check(&fs);
    assert!(v.bad.is_empty(), "{what}: {v:#?}");
    mid
}

fn start(fs: &Fs, k: &Keys, encrypt: bool) {
    if encrypt {
        let key = VolumeKey::from_master(&k.vk);
        fs.st().convert_encrypt_begin(key, &k.ring).unwrap();
    } else {
        fs.st().convert_decrypt_begin(&k.recovery).unwrap();
    }
}

/// Crash rounds converting `img` (`encrypt`: plain -> encrypted, else back), alternating
/// kills (whole writes) and power cuts (a torn one). Returns how many landed mid-way.
fn rounds(
    img: &[u8],
    k: &Keys,
    base: &BTreeMap<String, Vec<String>>,
    encrypt: bool,
    n: usize,
) -> usize {
    let mut mid = 0;
    for r in 0..n {
        let cut = if r % 2 == 0 {
            Cut::Writes(20 + rand() % 2500)
        } else {
            Cut::Sectors(50 + rand() % 40_000)
        };
        let disk = CrashDisk::new(img.to_vec(), cut);
        let fs = mount(disk.clone(), k);
        disk.arm();
        let converting = fs.st().converting();
        if !converting {
            let started = if encrypt {
                fs.st()
                    .convert_encrypt_begin(VolumeKey::from_master(&k.vk), &k.ring)
            } else {
                fs.st().convert_decrypt_begin(&k.recovery)
            };
            if let Err(e) = started {
                assert!(disk.dead(), "start: {e}");
            }
        }
        if !disk.dead() {
            convert(&fs, Some(&disk));
        }
        drop(fs);
        let what = std::format!(
            "{} round {r} ({cut:?})",
            if encrypt { "encrypt" } else { "decrypt" }
        );
        mid += check_round(&what, disk.image(), k, base, encrypt) as usize;
    }
    mid
}

#[test]
fn encrypt_and_decrypt_in_place_through_crashes() {
    setup(100);
    let (d, fs) = Fs::fresh(64);
    workload::run(&fs, 90);
    fs.commit().unwrap();
    fs.check_clean();
    let base = trees(&fs);
    assert!(
        base.len() >= 3,
        "snapshots to convert too: {:?}",
        base.keys()
    );
    let k = keys(fs.st().sb.uuid);
    drop(fs);
    let plain = d.image();
    assert!(
        contains(&plain, b"milkcrash i="),
        "the workload's plaintext is on the disk"
    );
    let mut mid = rounds(&plain, &k, &base, true, 8);

    // A fully encrypted copy to start decrypting from.
    let e = MemDisk::from_image(plain.clone());
    let fs = mount(e.clone(), &k);
    start(&fs, &k, true);
    assert!(convert(&fs, None));
    assert!(fs.st().store.encrypted());
    drop(fs);
    let enc = e.image();
    assert!(!contains(&enc, b"milkcrash i="));
    mid += rounds(&enc, &k, &base, false, 8);
    assert!(mid > 0, "no crash landed mid-way");
}

#[test]
fn what_is_refused_while_converting() {
    setup(101);
    let (_d, fs) = Fs::fresh(32);
    fs.write("/r5.bin", &noise(600_000)).unwrap();
    fs.st().snap_create("s5").unwrap();
    let k = keys(fs.st().sb.uuid);
    start(&fs, &k, true);
    assert!(fs.st().converting());
    let ebusy = Some(crate::errno::Errno::EBUSY);
    assert_eq!(fs.reflink("/r5.bin", "/copy.bin").err(), ebusy, "reflink");
    assert_eq!(workload::rollback(&fs, "s5").err(), ebusy, "rollback");
    assert_eq!(
        fs.st()
            .convert_encrypt_begin(VolumeKey::from_master(&k.vk), &k.ring)
            .err(),
        ebusy,
        "a second encrypt"
    );
    assert!(take_reason().contains("already being converted"));
    // Writes go in, and end up encrypted.
    fs.write("/during.txt", b"made while encrypting").unwrap();
    assert!(fs.st().convert_text().is_some());
    assert!(convert(&fs, None));
    assert_eq!(fs.read("/during.txt").unwrap(), b"made while encrypting");
    fs.check_clean();
    fs.space_exact();
}

/// Known problem (on main too, same code): a conversion's batch commits can leave the
/// live tree's shared-block table not matching its references until the tree is done
/// (`check` reports it; finishing the conversion makes it right). converttest fails when
/// a crash lands in such a window. `cargo test ... -- --ignored` shows it here.
#[test]
#[ignore = "known: shared-block counts mid-conversion (see the comment)"]
fn every_committed_step_of_a_conversion_checks_clean() {
    for seed in 0..40u64 {
        setup(1000 + seed);
        let (_d, fs) = Fs::fresh(64);
        workload::run(&fs, 60 + seed * 7);
        fs.commit().unwrap();
        let k = keys(fs.st().sb.uuid);
        start(&fs, &k, true);
        loop {
            let r = fs.st().convert_step(64).unwrap();
            let c = fs.st().check(false).unwrap();
            assert!(c.clean(), "seed {seed}: {}", c.text());
            if r == Step::Done {
                break;
            }
        }
    }
}
