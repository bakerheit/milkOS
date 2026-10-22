//! The copy-on-write B+tree: splits as it grows, merges and rebalances as it shrinks, the
//! same items as a model throughout, across commits and remounts (the kernel self-test's
//! "B+tree" and `treetest` checks).

use std::collections::BTreeMap;
use std::vec::Vec;

use super::harness::*;
use crate::btree;
use crate::format::*;
use crate::items::LIVE;

const KIND: u8 = 200;
const OBJ: u64 = 0x7000_0000_0000_0000;

fn stats(fs: &Fs) -> btree::TreeStats {
    let st = fs.st();
    btree::stats(&st.store, st.sb.root).unwrap()
}

fn all(fs: &Fs) -> Vec<(Key, Vec<u8>)> {
    fs.st()
        .test_items(Key::new(OBJ, KIND, 0), Key::new(OBJ + 100_000, KIND + 1, 0))
        .unwrap()
}

#[test]
fn random_inserts_replaces_deletes_match_a_model() {
    setup(20);
    let (d, fs) = Fs::fresh(64);
    // (After a first commit: from then on there is a space map too.)
    fs.st().meta_dirty = true;
    fs.commit().unwrap();
    let base = fs.st().verify_space().unwrap().allocated;
    let mut model: BTreeMap<Key, Vec<u8>> = BTreeMap::new();
    {
        let mut st = fs.st();
        st.set_cur(LIVE).unwrap();
        for i in 0..6000u32 {
            let r = rand();
            let key = Key::new(OBJ + r % 400, KIND, (r >> 20) % 50);
            if r >> 40 & 3 == 0 {
                let had = model.remove(&key).is_some();
                assert_eq!(st.test_del(&key).unwrap(), had);
            } else {
                let len = (r >> 44) as usize % 300;
                let v: Vec<u8> = (0..len).map(|j| (j as u64 ^ r) as u8).collect();
                st.test_put(key, v.clone()).unwrap();
                model.insert(key, v);
            }
            if i % 700 == 0 {
                st.commit().unwrap();
            }
        }
        st.commit().unwrap();
    }
    assert!(all(&fs).into_iter().eq(model.clone()));
    let grown = stats(&fs);
    assert!(grown.height >= 2, "it split: {grown:?}");
    // The same after a remount (read back from disk, no cached nodes).
    drop(fs);
    let fs = Fs::mount(d.clone()).unwrap();
    assert!(all(&fs).into_iter().eq(model.clone()));
    // Delete all but every 20th: nothing underfull is left (merged or rebalanced).
    let keys: Vec<Key> = model.keys().copied().collect();
    {
        let mut st = fs.st();
        for (i, k) in keys.iter().enumerate() {
            if i % 20 != 0 {
                assert!(st.test_del(k).unwrap());
                model.remove(k);
            }
        }
        st.commit().unwrap();
    }
    assert!(all(&fs).into_iter().eq(model.clone()));
    let shrunk = stats(&fs);
    assert_eq!(shrunk.underfull, 0, "{shrunk:?}");
    assert!(shrunk.nodes < grown.nodes);
    {
        let mut st = fs.st();
        for k in model.keys() {
            assert!(st.test_del(k).unwrap());
        }
        st.commit().unwrap();
    }
    assert!(all(&fs).is_empty());
    let s = fs.st().verify_space().unwrap();
    assert_eq!(
        (s.allocated, s.leaked, s.free_but_used),
        (base, 0, 0),
        "back to the same size, space exact"
    );
    fs.check_clean();
}

#[test]
fn many_files_fill_the_tree_and_deleting_them_shrinks_it() {
    // treetest: mass create, tree well filled; delete nearly all, it shrinks back.
    setup(21);
    let (_d, fs) = Fs::fresh(64);
    fs.mkdir("/tt").unwrap();
    let dir = fs.resolve(LIVE, "/tt").unwrap();
    for i in 0..3000 {
        fs.txn(LIVE, |st| {
            st.create_node(dir, &std::format!("file-{i:05}"), 0o100644, None)
        })
        .unwrap();
    }
    fs.commit().unwrap();
    let full = stats(&fs);
    assert!(full.fill_pct() >= 60, "{}% full", full.fill_pct());
    assert!(full.height >= 3, "{full:?}");
    for i in 0..3000 {
        if i % 100 != 0 {
            fs.txn(LIVE, |st| {
                st.unlink_node(dir, &std::format!("file-{i:05}"), &none_open)
            })
            .unwrap();
        }
    }
    fs.commit().unwrap();
    let small = stats(&fs);
    assert_eq!(small.underfull, 0);
    assert!(small.height < full.height && small.nodes * 10 < full.nodes);
    assert_eq!(fs.ls("/tt").unwrap().len(), 30);
    fs.check_clean();
    fs.space_exact();
}

#[test]
fn big_values_and_a_full_leaf() {
    setup(22);
    let (_d, fs) = Fs::fresh(16);
    let mut st = fs.st();
    // Items as big as allowed, enough to need several leaves each split.
    for i in 0..40u64 {
        st.test_put(Key::new(OBJ, KIND, i), std::vec![i as u8; MAX_VALUE])
            .unwrap();
    }
    st.commit().unwrap();
    for i in 0..40u64 {
        let v = st
            .test_items(Key::new(OBJ, KIND, i), Key::new(OBJ, KIND, i + 1))
            .unwrap();
        assert_eq!(v[0].1, std::vec![i as u8; MAX_VALUE]);
    }
    let s = btree::stats(&st.store, st.sb.root).unwrap();
    assert!(s.leaves >= 14, "{s:?}");
    drop(st);
    fs.check_clean();
}

#[test]
fn copy_on_write_never_touches_committed_nodes() {
    setup(23);
    let (d, fs) = Fs::fresh(16);
    for i in 0..200 {
        fs.write(&std::format!("/f{i}"), b"x").unwrap();
    }
    fs.commit().unwrap();
    let root = fs.st().sb.root;
    let before = d.image();
    let mut nodes = Vec::new();
    btree::walk(
        &fs.st().store,
        root,
        &mut |p| {
            nodes.push(p.block);
            true
        },
        &mut |_, _| Ok(()),
    )
    .unwrap();
    for i in 0..200 {
        fs.write(&std::format!("/f{i}"), b"changed").unwrap();
    }
    fs.commit().unwrap();
    // Every node of the old tree is still exactly as it was (or freed and reused only
    // after the commit that dropped it: here the space map moved them to pending).
    let after = d.image();
    let changed = nodes
        .iter()
        .filter(|&&b| before[b as usize * BLOCK..][..BLOCK] != after[b as usize * BLOCK..][..BLOCK])
        .count();
    assert_eq!(changed, 0, "a committed node was overwritten in place");
}
