//! Encrypting a plain volume in place, and decrypting an encrypted one back
//! (`snap encrypt` / `snap decrypt`, docs/filesystem.md "Encrypting in place").
//!
//! **Copy-on-write, a leaf at a time.** While `SB_CONVERTING` is set a block may be in
//! either form; a pointer says which (its checksum is the plaintext xxh64 or the SIV tag,
//! `Store::verify_mode` tries both), and every new block is written in the target form.
//! Conversion walks each tree a leaf at a time from a cursor: a leaf in the old form is
//! rewritten (copy-on-write: it and its path to the root are written anew, in the new
//! form, at the next commit), and every data block it points to in the old form is copied
//! to a new block in the new form, keeping its birth (so the release rules, which go by
//! birth, keep meaning the same). Each batch ends in an ordinary commit, which also
//! records the cursor (`K_CONVERT`): a crash at any point leaves some committed
//! generation, a consistent volume with some blocks converted, and the work resumes from
//! the cursor. Nothing is ever overwritten in place.
//!
//! **Sharing.** Snapshots and clones share data blocks with the live tree (and reflink
//! copies with each other). A copied data block is recorded in the remap table
//! (`K_REMAP` items: old block and checksum -> new block and checksum, in the live tree,
//! committed with the batch), and every other tree still pointing at the old block is
//! pointed at the same copy when its turn comes, so the data stays shared and is stored
//! once. Tree nodes aren't shared afterwards (each tree gets its own copies: a few percent
//! of the data per snapshot). Shared-block counts (`K_BREF`) move with the blocks in the
//! live tree and clones; a snapshot's are recounted when it is done. The volume works in
//! branched mode while converting (no dead lists; old blocks no tree needs any more are
//! reclaimed by reachability sweeps after each tree, and whenever space runs low).
//!
//! **Order**: the live tree, the snapshots (oldest first), the clones; then a pass that
//! checks every block of every tree is in the new form (a tree is redone if not); then,
//! when encrypting, free space is overwritten with zeros (old plaintext in freed blocks);
//! then the superblock flips (`SB_ENCRYPTED` on or off) and the remap table goes.
//! Meanwhile the volume is usable; what could add a new reference to an old-form block
//! (reflink copies, dedupe, rollback, clone, promote, receive) is refused with EBUSY.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use super::btree::{self, Child};
use super::crypt::VolumeKey;
use super::format::*;
use super::items::{LIVE, clone_view};
use super::volume::State;
use crate::fs::errno::{Errno, Result};

const PH_LIVE: u8 = 0;
const PH_SNAP: u8 = 1;
const PH_CLONE: u8 = 2;
const PH_VERIFY: u8 = 3;
const PH_WIPE: u8 = 4;

/// What a running conversion keeps in memory.
pub struct ConvRun {
    pub state: ConvertState,
    /// Old data block -> (its old checksum, new block, new checksum).
    remap: BTreeMap<u64, (u64, u64, u64)>,
    /// Remap entries of this batch, still to be written to the live tree.
    pending: Vec<(u64, (u64, u64, u64))>,
    /// The snapshot tree being rewritten (written back at the end of each batch).
    snap_tree: Option<Child>,
    /// The verify pass found blocks still in the old form.
    left: u64,
    /// Internal nodes the verify pass has seen in the new form.
    checked: BTreeSet<u64>,
    /// Why it isn't making progress (shown in `info`), e.g. not enough free space.
    pub paused: Option<String>,
}

/// One step's outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Did some work; more to do.
    More,
    /// The conversion is finished (the superblock flipped).
    Done,
    /// Can't go on now (see `paused`): needs this many more free blocks.
    NeedSpace(u64),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tree {
    Live,
    Snap(u64),
    Clone(u32),
}

fn remap_val(v: &(u64, u64, u64)) -> Vec<u8> {
    let mut b = Vec::with_capacity(24);
    for x in [v.0, v.1, v.2] {
        b.extend_from_slice(&x.to_le_bytes());
    }
    b
}

fn remap_decode(b: &[u8]) -> Option<(u64, u64, u64)> {
    (b.len() == 24).then(|| {
        let w = |i: usize| u64::from_le_bytes(b[i * 8..i * 8 + 8].try_into().unwrap());
        (w(0), w(1), w(2))
    })
}

/// Shared-block runs from per-block reference counts: (start, len, extra).
fn bref_runs(refs: &BTreeMap<u64, u32>) -> Vec<(u64, u64, u32)> {
    let mut out: Vec<(u64, u64, u32)> = Vec::new();
    for (&b, &n) in refs.iter().filter(|(_, n)| **n > 1) {
        let x = n - 1;
        match out.last_mut() {
            Some((s, l, rx)) if *s + *l == b && *rx == x && *l < MAX_RUN => *l += 1,
            _ => out.push((b, 1, x)),
        }
    }
    out
}

impl State {
    pub fn converting(&self) -> bool {
        self.flags & SB_CONVERTING != 0
    }

    /// Refuse what could add a reference to an old-form block while converting.
    pub fn not_converting(&self) -> Result<()> {
        if self.converting() {
            return super::fail(
                Errno::EBUSY,
                "the volume is being converted (encrypted or decrypted in place): wait for it to finish",
            );
        }
        Ok(())
    }

    /// After mounting a converting volume: its state and remap table from the live tree.
    pub fn conv_load(&mut self) -> Result<()> {
        if !self.converting() {
            return Ok(());
        }
        let mut items = Vec::new();
        self.view(LIVE, |r, s| {
            btree::range(
                r,
                s,
                &Key::new(VOL_OBJ, K_CONVERT, 0),
                &Key::new(VOL_OBJ, K_REMAP + 1, 0),
                &mut items,
            )
        })?;
        let mut state = ConvertState::default();
        let mut remap = BTreeMap::new();
        for (k, v) in items {
            match k.kind {
                K_CONVERT => state = ConvertState::decode(&v).ok_or(Errno::EIO)?,
                K_REMAP => {
                    remap.insert(k.off, remap_decode(&v).ok_or(Errno::EIO)?);
                }
                _ => {}
            }
        }
        self.conv = Some(ConvRun {
            state,
            remap,
            pending: Vec::new(),
            snap_tree: None,
            left: 0,
            checked: BTreeSet::new(),
            paused: None,
        });
        Ok(())
    }

    /// Why this volume can't start converting now, if it can't.
    fn convert_ok(&self) -> Result<()> {
        self.check_writable()?;
        if self.converting() {
            return super::fail(Errno::EBUSY, "it is already being converted");
        }
        if self.receiving || !self.busy_snaps.is_empty() {
            return super::fail(Errno::EBUSY, "a send or receive is using it");
        }
        Ok(())
    }

    /// Start encrypting in place under `key`, whose keyring (wrapped slots, made by the
    /// caller outside the volume lock: scrypt is slow) is `ring`.
    pub fn convert_encrypt_begin(&mut self, key: VolumeKey, ring: &Keyring) -> Result<()> {
        self.convert_ok()?;
        if self.store.crypt.is_some() {
            return super::fail(Errno::EINVAL, "it is encrypted already");
        }
        if ring.uuid != self.sb.uuid {
            return Err(Errno::EINVAL);
        }
        self.commit()?;
        let p = self.store.write_plain(&ring.encode())?;
        self.convert_begin(true, Arc::new(key), p)
    }

    /// Start decrypting in place; `secret` (a passphrase or the recovery key) must open
    /// the volume's keyring to its key (EACCES otherwise).
    pub fn convert_decrypt_begin(&mut self, secret: &str) -> Result<()> {
        self.convert_ok()?;
        let Some(key) = self.store.crypt.clone() else {
            return super::fail(Errno::EINVAL, "it isn't encrypted");
        };
        let ring = self.read_keyring()?;
        let (k2, _) = match super::crypt::unlock(&ring, secret) {
            Ok(k) => k,
            Err(Errno::EACCES) => {
                return super::fail(Errno::EACCES, "wrong passphrase or recovery key");
            }
            Err(e) => return Err(e),
        };
        if !k2.same_as(&key) {
            return super::fail(Errno::EACCES, "wrong passphrase or recovery key");
        }
        self.commit()?;
        self.convert_begin(false, key, self.keyring)
    }

    /// Start converting (the open transaction is committed). Encrypting: `key` is the new
    /// volume key and `keyring` its keyring block (written, plaintext, not yet
    /// referenced); decrypting: the volume's own key and keyring.
    fn convert_begin(
        &mut self,
        to_sealed: bool,
        key: Arc<VolumeKey>,
        keyring: BlockPtr,
    ) -> Result<()> {
        let used = self.store.total() - self.store.free_blocks();
        self.store.crypt = Some(key);
        self.store.convert = Some(to_sealed);
        self.keyring = keyring;
        self.flags = (self.flags | SB_CONVERTING) & !SB_ENCRYPTED & !SB_TO_PLAIN;
        if !to_sealed {
            self.flags |= SB_TO_PLAIN;
        }
        self.version = self.version.max(CRYPT_VERSION);
        // No dead lists while blocks move: sweeps reclaim (see the module docs).
        self.set_branched();
        let state = ConvertState {
            total: used,
            started: crate::fs::clock::now().sec.max(0) as u64,
            ..ConvertState::default()
        };
        self.conv = Some(ConvRun {
            state,
            remap: BTreeMap::new(),
            pending: Vec::new(),
            snap_tree: None,
            left: 0,
            checked: BTreeSet::new(),
            paused: None,
        });
        self.conv_save()?;
        self.meta_dirty = true;
        self.commit()
    }

    /// Write the conversion state and this batch's remap entries into the live tree.
    fn conv_save(&mut self) -> Result<()> {
        let saved = self.cur;
        self.set_cur(LIVE)?;
        let (state, pending) = {
            let c = self.conv.as_mut().ok_or(Errno::EINVAL)?;
            (c.state, core::mem::take(&mut c.pending))
        };
        let r = (|| {
            for (old, v) in pending {
                self.put(Key::new(VOL_OBJ, K_REMAP, old), remap_val(&v))?;
            }
            self.put(Key::new(VOL_OBJ, K_CONVERT, 0), state.encode())
        })();
        self.set_cur(saved)?;
        r
    }

    fn tree_root_ptr(&self, t: Tree) -> Option<BlockPtr> {
        match t {
            Tree::Snap(g) => self
                .snaps
                .iter()
                .find(|s| s.generation == g)
                .map(|s| s.root),
            _ => None,
        }
    }

    fn leaf(&self, t: Tree, key: &Key) -> Result<btree::LeafAt> {
        match t {
            Tree::Live => btree::leaf_at(&self.tree, &self.store, key),
            Tree::Clone(id) => self.view(clone_view(id), |r, s| btree::leaf_at(r, s, key)),
            Tree::Snap(_) => {
                let c = self.conv.as_ref().ok_or(Errno::EINVAL)?;
                match &c.snap_tree {
                    Some(ch) => btree::leaf_at(ch, &self.store, key),
                    None => {
                        let p = self.tree_root_ptr(t).ok_or(Errno::ENOENT)?;
                        btree::leaf_at(&Child::Disk(p), &self.store, key)
                    }
                }
            }
        }
    }

    fn tput(&mut self, t: Tree, k: Key, v: Vec<u8>) -> Result<()> {
        match t {
            Tree::Snap(_) => {
                let mut ch = self.snap_child(t)?;
                let r = btree::insert(&mut ch, &mut self.store, k, v);
                self.conv.as_mut().unwrap().snap_tree = Some(ch);
                r
            }
            _ => self.put(k, v),
        }
    }

    fn tdel(&mut self, t: Tree, k: &Key) -> Result<()> {
        match t {
            Tree::Snap(_) => {
                let mut ch = self.snap_child(t)?;
                let r = btree::remove(&mut ch, &mut self.store, k);
                self.conv.as_mut().unwrap().snap_tree = Some(ch);
                r.map(|_| ())
            }
            _ => self.del(k).map(|_| ()),
        }
    }

    fn snap_child(&mut self, t: Tree) -> Result<Child> {
        let root = self.tree_root_ptr(t).ok_or(Errno::ENOENT)?;
        let c = self.conv.as_mut().ok_or(Errno::EINVAL)?;
        Ok(c.snap_tree.take().unwrap_or(Child::Disk(root)))
    }

    /// Write the snapshot tree being rewritten and point its record at it.
    fn snap_flush(&mut self, t: Tree) -> Result<()> {
        let Tree::Snap(g) = t else { return Ok(()) };
        let Some(mut ch) = self.conv.as_mut().and_then(|c| c.snap_tree.take()) else {
            return Ok(());
        };
        if ch.is_dirty() {
            let p = btree::write(&mut ch, &mut self.store)?;
            if let Some(s) = self.snaps.iter_mut().find(|s| s.generation == g) {
                s.root = p;
                self.meta_dirty = true;
            }
        }
        self.conv.as_mut().unwrap().snap_tree = Some(ch);
        Ok(())
    }

    /// Recount a snapshot's shared-block table from its references (its blocks moved).
    fn snap_recount(&mut self, t: Tree) -> Result<()> {
        self.snap_flush(t)?;
        let root = self.tree_root_ptr(t).ok_or(Errno::ENOENT)?;
        let mut refs: BTreeMap<u64, u32> = BTreeMap::new();
        let mut old = Vec::new();
        btree::walk(&self.store, root, &mut |_| true, &mut |k, v| {
            if k.obj == VOL_OBJ && k.kind == K_BREF {
                old.push(*k);
            }
            for p in data_ptrs(k, v) {
                *refs.entry(p.block).or_default() += 1;
            }
            Ok(())
        })?;
        for k in old {
            self.tdel(t, &k)?;
        }
        for (s, l, x) in bref_runs(&refs) {
            let mut v = Vec::with_capacity(8);
            v.extend_from_slice(&(l as u32).to_le_bytes());
            v.extend_from_slice(&x.to_le_bytes());
            self.tput(t, Key::new(VOL_OBJ, K_BREF, s), v)?;
        }
        self.snap_flush(t)
    }

    /// Bring run `e` (of a file in tree `t`) to the new form: blocks already converted for
    /// another tree are pointed at their copy, old-form ones are copied. Returns the new
    /// pieces (None: unchanged) and how many blocks were copied.
    fn convert_ext(&mut self, t: Tree, e: &Ext) -> Result<(Option<Vec<Ext>>, u64)> {
        let n = e.csums.len();
        let mut raw = alloc::vec![0u8; n * BLOCK];
        self.store.cache.read_run(e.block, &mut raw)?;
        let target = self.store.writes_sealed();
        let mut out: Vec<(u64, u64)> = Vec::with_capacity(n);
        let mut copy: Vec<usize> = Vec::new();
        let mut remapped: Vec<usize> = Vec::new();
        {
            let c = self.conv.as_ref().ok_or(Errno::EINVAL)?;
            for i in 0..n {
                let b = e.block + i as u64;
                if let Some(&(oc, nb, nc)) = c.remap.get(&b)
                    && oc == e.csums[i]
                {
                    out.push((nb, nc));
                    remapped.push(i);
                    continue;
                }
                let d = &mut raw[i * BLOCK..(i + 1) * BLOCK];
                match self.store.verify_mode(b, e.birth, e.csums[i], d) {
                    Some(sealed) if sealed == target => out.push((b, e.csums[i])),
                    Some(_) => {
                        out.push((0, 0));
                        copy.push(i);
                    }
                    None => {
                        crate::kprintln!("milkfs: convert: checksum mismatch in block {b}");
                        return Err(Errno::EIO);
                    }
                }
            }
        }
        if copy.is_empty() && remapped.is_empty() {
            return Ok((None, 0));
        }
        let writable = !matches!(t, Tree::Snap(_));
        // Drop this tree's reference to each old block (unless the tree shares it with
        // another of its files: then the count moves to the copy).
        let mut moved: Vec<(usize, usize, u64)> = Vec::new(); // (first index, len, new block)
        let mut copied = 0u64;
        let mut i = 0;
        while i < copy.len() {
            let mut j = i;
            while j + 1 < copy.len() && copy[j + 1] == copy[j] + 1 {
                j += 1;
            }
            // copy[i..=j] is a contiguous range of the run
            let (mut a, b) = (copy[i], copy[j] + 1);
            while a < b {
                let (first, got) = self.store.alloc_run(b - a)?;
                for q in 0..got {
                    let idx = a + q;
                    let csum = self.store.write_copy(
                        first + q as u64,
                        e.birth,
                        &raw[idx * BLOCK..(idx + 1) * BLOCK],
                    )?;
                    out[idx] = (first + q as u64, csum);
                    let ent = (e.csums[idx], first + q as u64, csum);
                    let c = self.conv.as_mut().unwrap();
                    c.remap.insert(e.block + idx as u64, ent);
                    c.pending.push((e.block + idx as u64, ent));
                }
                moved.push((a, got, first));
                copied += got as u64;
                a += got;
            }
            i = j + 1;
        }
        // Remapped blocks: their counts in this tree move too.
        let mut k = 0;
        while k < remapped.len() {
            let mut l = k;
            while l + 1 < remapped.len()
                && remapped[l + 1] == remapped[l] + 1
                && out[remapped[l + 1]].0 == out[remapped[l]].0 + 1
            {
                l += 1;
            }
            moved.push((remapped[k], l - k + 1, out[remapped[k]].0));
            k = l + 1;
        }
        if writable {
            for &(a, len, to) in &moved {
                let old = e.block + a as u64;
                for q in 0..len {
                    let b = old + q as u64;
                    if !self.bref_any(b, b + 1)? {
                        self.store.release(e.ptr(a + q));
                    }
                }
                self.bref_move(old, len as u64, to)?;
            }
        }
        // The new pieces: split where the blocks stop being consecutive.
        let mut pieces: Vec<Ext> = Vec::new();
        for (idx, (b, c)) in out.into_iter().enumerate() {
            match pieces.last_mut() {
                Some(p)
                    if p.block + p.csums.len() as u64 == b && p.csums.len() < MAX_RUN as usize =>
                {
                    p.csums.push(c)
                }
                _ => pieces.push(Ext {
                    start: e.start + idx as u64,
                    block: b,
                    birth: e.birth,
                    csums: alloc::vec![c],
                    legacy: false,
                }),
            }
        }
        Ok((Some(pieces), copied))
    }

    /// Convert the leaf of tree `t` at the cursor. Returns the next cursor (None: the
    /// tree is done) and the blocks written.
    fn convert_leaf(&mut self, t: Tree, cursor: Key) -> Result<(Option<Key>, u64)> {
        let leaf = self.leaf(t, &cursor)?;
        let old_leaf = match leaf.ptr {
            Some(p) => !self.store.in_target_form(p)?,
            None => false, // dirty: written in the new form at the next commit
        };
        // Item key -> what replaces it (nothing: deleted).
        type Items = Vec<(Key, Vec<u8>)>;
        let mut changes: Vec<(Key, Items)> = Vec::new();
        let mut written = 0;
        for (k, v) in &leaf.items {
            if *k < cursor {
                continue;
            }
            if t != Tree::Live && k.obj == VOL_OBJ && (k.kind == K_CONVERT || k.kind == K_REMAP) {
                // A snapshot taken while converting has copies of these: not its own.
                changes.push((*k, Vec::new()));
                continue;
            }
            if let Some(e) = Ext::decode(k, v)
                && let (Some(pieces), n) = self.convert_ext(t, &e)?
            {
                written += n;
                changes.push((
                    *k,
                    pieces.iter().map(|p| (p.key(k.obj), p.encode())).collect(),
                ));
            }
        }
        let first = leaf.items.first().cloned();
        for (k, new) in changes.iter() {
            self.tdel(t, k)?;
            for (nk, nv) in new {
                self.tput(t, *nk, nv.clone())?;
            }
        }
        if old_leaf
            && changes.is_empty()
            && let Some((k, v)) = first
        {
            // Rewrite it as it is (copy-on-write: in the new form, path included).
            self.tput(t, k, v)?;
        }
        if old_leaf || !changes.is_empty() {
            written += 1;
        }
        Ok((leaf.next, written))
    }

    /// The tree after `t` in conversion order, and its phase.
    fn next_tree(&self, phase: u8, tree: u64) -> (u8, u64) {
        if phase <= PH_SNAP {
            let after = if phase == PH_LIVE { 0 } else { tree };
            if let Some(g) = self
                .snaps
                .iter()
                .map(|s| s.generation)
                .filter(|&g| g > after)
                .min()
            {
                return (PH_SNAP, g);
            }
        }
        let after = if phase == PH_CLONE { tree } else { 0 };
        if phase <= PH_CLONE
            && let Some(id) = self
                .clones
                .iter()
                .map(|c| c.id as u64)
                .filter(|&id| id > after)
                .min()
        {
            return (PH_CLONE, id);
        }
        (PH_VERIFY, 0)
    }

    fn tree_for(&self, phase: u8, tree: u64) -> Option<Tree> {
        match phase {
            PH_LIVE => Some(Tree::Live),
            PH_SNAP => self
                .snaps
                .iter()
                .any(|s| s.generation == tree)
                .then_some(Tree::Snap(tree)),
            PH_CLONE => self
                .clones
                .iter()
                .any(|c| c.id as u64 == tree)
                .then_some(Tree::Clone(tree as u32)),
            _ => None,
        }
    }

    /// Every tree, for the verify pass: (phase, tree id).
    fn all_trees(&self) -> Vec<(u8, u64)> {
        let mut v = alloc::vec![(PH_LIVE, 0)];
        let mut g: Vec<u64> = self.snaps.iter().map(|s| s.generation).collect();
        g.sort_unstable();
        v.extend(g.into_iter().map(|g| (PH_SNAP, g)));
        let mut c: Vec<u64> = self.clones.iter().map(|c| c.id as u64).collect();
        c.sort_unstable();
        v.extend(c.into_iter().map(|c| (PH_CLONE, c)));
        v
    }

    /// Verify one leaf of tree `t` (and the internal nodes above it): every block in the
    /// new form? Counts what isn't in `left`.
    fn verify_leaf(&mut self, t: Tree, cursor: Key) -> Result<Option<Key>> {
        let leaf = self.leaf(t, &cursor)?;
        let mut left = 0;
        for p in &leaf.path {
            if self.conv.as_ref().unwrap().checked.contains(&p.block) {
                continue;
            }
            if self.store.in_target_form(*p)? {
                self.conv.as_mut().unwrap().checked.insert(p.block);
            } else {
                left += 1;
            }
        }
        if let Some(p) = leaf.ptr
            && !self.store.in_target_form(p)?
        {
            left += 1;
        }
        for (k, v) in &leaf.items {
            if *k < cursor {
                continue;
            }
            for p in data_ptrs(k, v) {
                if !self.store.in_target_form(p)? {
                    left += 1;
                }
            }
        }
        self.conv.as_mut().unwrap().left += left;
        Ok(leaf.next)
    }

    /// One batch of conversion work (at most about `budget` blocks written), ending in a
    /// commit that records where it got to.
    pub fn convert_step(&mut self, budget: u64) -> Result<Step> {
        let Some(c) = self.conv.as_ref() else {
            return Err(Errno::EINVAL);
        };
        let mut st = c.state;
        // Room for this batch (copies + new nodes): sweep first if it's getting tight.
        let need = budget * 2 + 64;
        if st.phase < PH_VERIFY && self.store.free_blocks() < need.max(self.store.total() / 10) {
            self.sweep()?;
            if self.store.free_blocks() < need {
                let short = need - self.store.free_blocks();
                self.conv.as_mut().unwrap().paused = Some(alloc::format!(
                    "paused: needs {} more free KiB (delete files or snapshots)",
                    short * 4
                ));
                return Ok(Step::NeedSpace(short));
            }
        }
        self.conv.as_mut().unwrap().paused = None;
        let mut written = 0u64; // blocks rewritten (progress)
        let mut work = 0u64; // verify reads, free blocks cleared (batch size only)
        let mut sweep_after = false;
        let r = (|| -> Result<Step> {
            loop {
                match st.phase {
                    PH_LIVE | PH_SNAP | PH_CLONE => {
                        let Some(t) = self.tree_for(st.phase, st.tree) else {
                            // That snapshot or clone went away meanwhile.
                            (st.phase, st.tree) = self.next_tree(st.phase, st.tree);
                            st.cursor = Key::MIN;
                            self.conv.as_mut().unwrap().snap_tree = None;
                            continue;
                        };
                        if let Tree::Clone(id) = t {
                            self.set_cur(clone_view(id))?;
                        } else {
                            self.set_cur(LIVE)?;
                        }
                        let (next, n) = self.convert_leaf(t, st.cursor)?;
                        written += n;
                        match next {
                            Some(k) => st.cursor = k,
                            None => {
                                if let Tree::Snap(_) = t {
                                    self.snap_recount(t)?;
                                    self.conv.as_mut().unwrap().snap_tree = None;
                                }
                                self.set_cur(LIVE)?;
                                (st.phase, st.tree) = self.next_tree(st.phase, st.tree);
                                st.cursor = Key::MIN;
                                if st.phase == PH_VERIFY {
                                    let c = self.conv.as_mut().unwrap();
                                    c.left = 0;
                                    c.checked.clear();
                                }
                                // Reclaim what that tree no longer needs (after the commit).
                                sweep_after = true;
                                return Ok(Step::More);
                            }
                        }
                    }
                    PH_VERIFY => {
                        // `tree` holds the tree being verified: its phase << 56 | its id.
                        let at = ((st.tree >> 56) as u8, st.tree & ((1 << 56) - 1));
                        let trees = self.all_trees();
                        let Some(i) = trees.iter().position(|&x| x >= at) else {
                            return self.verify_done(&mut st);
                        };
                        let (p, id) = trees[i];
                        if (p, id) != at {
                            st.cursor = Key::MIN; // that one went away: the next from its start
                        }
                        let t = self.tree_for(p, id).ok_or(Errno::EIO)?;
                        let next = self.verify_leaf(t, st.cursor)?;
                        work += 1;
                        match next {
                            Some(k) => {
                                st.tree = (p as u64) << 56 | id;
                                st.cursor = k;
                            }
                            None if i + 1 < trees.len() => {
                                let (np, nid) = trees[i + 1];
                                st.tree = (np as u64) << 56 | nid;
                                st.cursor = Key::MIN;
                            }
                            None => return self.verify_done(&mut st),
                        }
                    }
                    PH_WIPE => {
                        // Old plaintext in blocks that are free now: overwrite it.
                        let zero = [0u8; BLOCK];
                        let mut b = st.cursor.off.max(2);
                        let end = self.store.total();
                        let mut n = 0;
                        while b < end && n < budget * 4 {
                            if !self.store.is_used(b) {
                                self.store.cache.write(b, &zero)?;
                                n += 1;
                            }
                            b += 1;
                        }
                        work += n / 4;
                        st.cursor = Key::new(0, 0, b);
                        if b >= end {
                            self.store.cache.flush()?;
                            self.conv.as_mut().unwrap().state = st;
                            return self.convert_finish();
                        }
                    }
                    _ => return Err(Errno::EIO),
                }
                if written + work >= budget {
                    return Ok(Step::More);
                }
            }
        })();
        let fin = matches!(r, Ok(Step::Done));
        let _ = self.set_cur(LIVE);
        if fin {
            return r;
        }
        if let Err(e) = r {
            crate::kprintln!("milkfs: convert: a batch failed: {e}");
            return Err(e);
        }
        if let Some(c) = self.conv.as_mut() {
            st.done += written;
            c.state = st;
        }
        if let Some(t) = self.tree_for(st.phase, st.tree) {
            self.snap_flush(t)?;
        }
        self.conv_save()?;
        self.commit()?;
        if sweep_after {
            self.sweep()?;
        }
        r
    }

    /// The verify pass is through: another pass if it found old-form blocks, else on to
    /// clearing free space (encrypting) or the end.
    fn verify_done(&mut self, st: &mut ConvertState) -> Result<Step> {
        let left = self.conv.as_ref().unwrap().left;
        if left > 0 {
            crate::kprintln!(
                "milkfs: convert: {left} block(s) still in the old form: another pass"
            );
            st.passes += 1;
            st.phase = PH_LIVE;
            st.tree = 0;
        } else if self.store.writes_sealed() {
            st.phase = PH_WIPE;
            st.tree = 0;
        } else {
            self.conv.as_mut().unwrap().state = *st;
            return self.convert_finish();
        }
        st.cursor = Key::MIN;
        self.conv.as_mut().unwrap().state = *st;
        self.conv_save()?;
        // Old blocks nothing needs go now (before free space is cleared).
        self.sweep()?;
        Ok(Step::More)
    }

    /// Everything is in the new form: flip the superblock, drop the remap table.
    fn convert_finish(&mut self) -> Result<Step> {
        let to_sealed = self.store.writes_sealed();
        let mut items = Vec::new();
        self.view(LIVE, |r, s| {
            btree::range(
                r,
                s,
                &Key::new(VOL_OBJ, K_CONVERT, 0),
                &Key::new(VOL_OBJ, K_REMAP + 1, 0),
                &mut items,
            )
        })?;
        self.set_cur(LIVE)?;
        for (k, _) in items {
            self.del(&k)?;
        }
        self.flags &= !(SB_CONVERTING | SB_TO_PLAIN);
        self.store.convert = None;
        if to_sealed {
            self.flags |= SB_ENCRYPTED;
        } else {
            self.flags &= !SB_ENCRYPTED;
            self.store.crypt = None;
            self.store
                .free_after_commit(self.keyring.block, self.keyring.birth);
            self.keyring = BlockPtr::NONE;
            if self.version == CRYPT_VERSION {
                self.version = REFLINK_VERSION;
            }
        }
        let done = self.conv.take().map(|c| c.state);
        self.meta_dirty = true;
        self.commit()?;
        self.maybe_unbranch();
        self.sweep()?;
        if let Some(s) = done {
            crate::kprintln!(
                "milkfs: {} done: {} block(s) rewritten{}",
                if to_sealed {
                    "encryption"
                } else {
                    "decryption"
                },
                s.done,
                if s.passes > 0 {
                    alloc::format!(", {} extra pass(es)", s.passes)
                } else {
                    String::new()
                }
            );
        }
        Ok(Step::Done)
    }

    /// The conversion's progress for `info` (None: not converting), machine-readable:
    /// `encrypt|decrypt PCT% (DONE/TOTAL blocks), PHASE[, paused: REASON]`, PHASE one of
    /// `live tree`, `snapshot NAME`, `clone NAME`, `verifying`, `clearing free space`.
    /// TOTAL is the blocks in use when it started; DONE may pass it (snapshots get their
    /// own tree nodes), PCT never says 100 before it is done.
    pub fn convert_text(&self) -> Option<String> {
        let c = self.conv.as_ref()?;
        let s = &c.state;
        let what = if self.store.writes_sealed() {
            "encrypt"
        } else {
            "decrypt"
        };
        let phase = match s.phase {
            PH_LIVE => String::from("live tree"),
            PH_SNAP => alloc::format!(
                "snapshot {}",
                self.snaps
                    .iter()
                    .find(|x| x.generation == s.tree)
                    .map_or("?", |x| x.name.as_str())
            ),
            PH_CLONE => alloc::format!(
                "clone {}",
                self.clones
                    .iter()
                    .find(|x| x.id as u64 == s.tree)
                    .map_or("?", |x| x.name.as_str())
            ),
            PH_VERIFY => String::from("verifying"),
            _ => String::from("clearing free space"),
        };
        // Rewriting is most of it (snapshots add their own tree nodes, so "about");
        // verifying and clearing free space are the last few percent.
        let pct = match s.phase {
            PH_VERIFY => 95,
            PH_WIPE => 96 + 3 * s.cursor.off / self.store.total().max(1),
            _ => (s.done * 95 / s.total.max(1)).min(94),
        };
        Some(alloc::format!(
            "{what} {pct}% ({}/{} blocks), {phase}{}",
            s.done,
            s.total,
            c.paused
                .as_deref()
                .map(|p| alloc::format!(", {p}"))
                .unwrap_or_default()
        ))
    }
}
