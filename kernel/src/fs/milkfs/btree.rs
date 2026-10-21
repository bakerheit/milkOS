//! Copy-on-write B+tree over [`Store`].
//!
//! A tree is a [`Child`]: either a committed on-disk node (`Disk`) or a node modified in
//! the current transaction (`Mem`, always dirty). Changing anything loads the path from the
//! root into memory, releasing the old blocks (by birth rules, see `store.rs`); nothing on
//! disk is touched until [`write`] serialises the dirty nodes bottom-up into new blocks at
//! commit. Read-only lookups never copy nodes into memory.
//!
//! Leaves hold `(Key, value)` items, internal nodes `(separator, child)`; child `i` covers
//! keys from its separator up to the next one (child 0's separator is ignored). Nodes split
//! when their encoding outgrows a block. Deletion drops empty nodes but does not rebalance
//! (v0: underfull nodes are allowed).

use alloc::boxed::Box;
use alloc::vec::Vec;

use super::format::{
    BLOCK, BlockPtr, DiskNode, INTERNAL_ENTRY, K_EXTENT, Key, LEAF_ITEM_OVERHEAD, MAX_VALUE,
    NODE_HEADER, PTR_SIZE, decode_node, encode_internal, encode_leaf,
};
use super::store::Store;
use crate::fs::errno::{Errno, Result};

pub enum Node {
    Leaf(Vec<(Key, Vec<u8>)>),
    Internal(u8, Vec<(Key, Child)>),
}

pub enum Child {
    Disk(BlockPtr),
    Mem(Box<Node>),
}

impl Child {
    pub fn is_dirty(&self) -> bool {
        matches!(self, Child::Mem(_))
    }
}

/// An empty tree (a dirty empty leaf).
pub fn empty() -> Child {
    Child::Mem(Box::new(Node::Leaf(Vec::new())))
}

pub fn load(st: &Store, p: BlockPtr) -> Result<Node> {
    let mut buf = [0u8; BLOCK];
    st.read(p, &mut buf)?;
    Ok(match decode_node(&buf)? {
        DiskNode::Leaf(items) => Node::Leaf(items),
        DiskNode::Internal(level, kids) => Node::Internal(
            level,
            kids.into_iter().map(|(k, p)| (k, Child::Disk(p))).collect(),
        ),
    })
}

fn leaf_size(items: &[(Key, Vec<u8>)]) -> usize {
    NODE_HEADER
        + items
            .iter()
            .map(|(_, v)| LEAF_ITEM_OVERHEAD + v.len())
            .sum::<usize>()
}

fn internal_fits(n: usize) -> bool {
    NODE_HEADER + n * INTERNAL_ENTRY <= BLOCK
}

/// Index of the child whose range contains `key`.
fn child_index(kids: &[(Key, Child)], key: &Key) -> usize {
    kids.partition_point(|(k, _)| k <= key).saturating_sub(1)
}

// ---- reads --------------------------------------------------------------------------

fn with_node<R>(c: &Child, st: &Store, f: impl FnOnce(&Node) -> Result<R>) -> Result<R> {
    match c {
        Child::Mem(n) => f(n),
        Child::Disk(p) => f(&load(st, *p)?),
    }
}

pub fn get(c: &Child, st: &Store, key: &Key) -> Result<Option<Vec<u8>>> {
    with_node(c, st, |n| match n {
        Node::Leaf(items) => Ok(items
            .binary_search_by(|(k, _)| k.cmp(key))
            .ok()
            .map(|i| items[i].1.clone())),
        Node::Internal(_, kids) => {
            if kids.is_empty() {
                return Ok(None);
            }
            get(&kids[child_index(kids, key)].1, st, key)
        }
    })
}

/// Every item with `lo <= key < hi`, in key order.
pub fn range(
    c: &Child,
    st: &Store,
    lo: &Key,
    hi: &Key,
    out: &mut Vec<(Key, Vec<u8>)>,
) -> Result<()> {
    with_node(c, st, |n| match n {
        Node::Leaf(items) => {
            let start = items.partition_point(|(k, _)| k < lo);
            for (k, v) in &items[start..] {
                if k >= hi {
                    break;
                }
                out.push((*k, v.clone()));
            }
            Ok(())
        }
        Node::Internal(_, kids) => {
            if kids.is_empty() {
                return Ok(());
            }
            let first = child_index(kids, lo);
            for (i, (k, child)) in kids.iter().enumerate().skip(first) {
                if i > first && *k >= *hi {
                    break;
                }
                range(child, st, lo, hi, out)?;
            }
            Ok(())
        }
    })
}

// ---- writes -------------------------------------------------------------------------

/// Make `c` an in-memory (dirty) node, releasing its old block.
fn make_mut<'a>(c: &'a mut Child, st: &mut Store) -> Result<&'a mut Node> {
    if let Child::Disk(p) = *c {
        let n = load(st, p)?;
        st.release(p);
        *c = Child::Mem(Box::new(n));
    }
    match c {
        Child::Mem(n) => Ok(n),
        Child::Disk(_) => unreachable!(),
    }
}

/// Split an oversized leaf; returns the right half and its first key.
fn split_leaf(items: &mut Vec<(Key, Vec<u8>)>) -> (Key, Child) {
    let sizes: Vec<usize> = items
        .iter()
        .map(|(_, v)| LEAF_ITEM_OVERHEAD + v.len())
        .collect();
    let total: usize = sizes.iter().sum();
    // Split point that minimises the larger half.
    let (mut best, mut best_max, mut left) = (1, usize::MAX, 0);
    for (s, sz) in sizes.iter().enumerate().take(items.len() - 1) {
        left += sz;
        let m = left.max(total - left);
        if m < best_max {
            best_max = m;
            best = s + 1;
        }
    }
    let right = items.split_off(best);
    (right[0].0, Child::Mem(Box::new(Node::Leaf(right))))
}

fn insert_rec(
    c: &mut Child,
    st: &mut Store,
    key: Key,
    val: Vec<u8>,
) -> Result<Option<(Key, Child)>> {
    match make_mut(c, st)? {
        Node::Leaf(items) => {
            match items.binary_search_by(|(k, _)| k.cmp(&key)) {
                Ok(i) => items[i].1 = val,
                Err(i) => items.insert(i, (key, val)),
            }
            if leaf_size(items) > BLOCK {
                return Ok(Some(split_leaf(items)));
            }
            Ok(None)
        }
        Node::Internal(level, kids) => {
            let level = *level;
            let idx = if kids.is_empty() {
                kids.push((key, Child::Mem(Box::new(Node::Leaf(Vec::new())))));
                0
            } else {
                child_index(kids, &key)
            };
            if let Some(split) = insert_rec(&mut kids[idx].1, st, key, val)? {
                kids.insert(idx + 1, split);
            }
            if !internal_fits(kids.len()) {
                let right = kids.split_off(kids.len() / 2);
                let k = right[0].0;
                return Ok(Some((
                    k,
                    Child::Mem(Box::new(Node::Internal(level, right))),
                )));
            }
            Ok(None)
        }
    }
}

/// Insert or replace `key`.
pub fn insert(root: &mut Child, st: &mut Store, key: Key, val: Vec<u8>) -> Result<()> {
    if val.len() > MAX_VALUE {
        return Err(Errno::ERANGE);
    }
    if let Some((k, right)) = insert_rec(root, st, key, val)? {
        let level = match root {
            Child::Mem(n) => match **n {
                Node::Leaf(_) => 1,
                Node::Internal(l, _) => l + 1,
            },
            Child::Disk(_) => unreachable!(),
        };
        let left = core::mem::replace(root, empty());
        *root = Child::Mem(Box::new(Node::Internal(
            level,
            alloc::vec![(Key::MIN, left), (k, right)],
        )));
    }
    Ok(())
}

fn remove_rec(c: &mut Child, st: &mut Store, key: &Key) -> Result<bool> {
    // Don't dirty a path that doesn't hold the key.
    if !c.is_dirty() && get(c, st, key)?.is_none() {
        return Ok(false);
    }
    match make_mut(c, st)? {
        Node::Leaf(items) => match items.binary_search_by(|(k, _)| k.cmp(key)) {
            Ok(i) => {
                items.remove(i);
                Ok(true)
            }
            Err(_) => Ok(false),
        },
        Node::Internal(_, kids) => {
            if kids.is_empty() {
                return Ok(false);
            }
            let idx = child_index(kids, key);
            let found = remove_rec(&mut kids[idx].1, st, key)?;
            let empty = match &kids[idx].1 {
                Child::Mem(n) => match &**n {
                    Node::Leaf(items) => items.is_empty(),
                    Node::Internal(_, k) => k.is_empty(),
                },
                Child::Disk(_) => false,
            };
            if empty {
                kids.remove(idx); // dirty, so its old block was already released
            }
            Ok(found)
        }
    }
}

/// Remove `key`. Returns whether it was there.
pub fn remove(root: &mut Child, st: &mut Store, key: &Key) -> Result<bool> {
    let found = remove_rec(root, st, key)?;
    // Collapse a root with a single child.
    loop {
        let single = match root {
            Child::Mem(n) => matches!(&**n, Node::Internal(_, k) if k.len() == 1),
            Child::Disk(_) => false,
        };
        if !single {
            break;
        }
        if let Child::Mem(n) = core::mem::replace(root, empty())
            && let Node::Internal(_, mut k) = *n
        {
            *root = k.pop().unwrap().1;
        }
        if let Child::Disk(_) = root {
            // The surviving child is still clean; that's fine.
            break;
        }
    }
    Ok(found)
}

/// Serialise every dirty node of the tree into new blocks. Returns the root pointer.
pub fn write(c: &mut Child, st: &mut Store) -> Result<BlockPtr> {
    let ptr = match c {
        Child::Disk(p) => return Ok(*p),
        Child::Mem(n) => {
            let mut buf = [0u8; BLOCK];
            match &mut **n {
                Node::Leaf(items) => encode_leaf(items, st.txn, &mut buf),
                Node::Internal(level, kids) => {
                    let mut ptrs = Vec::with_capacity(kids.len());
                    for (k, child) in kids.iter_mut() {
                        let p = write(child, st)?;
                        *child = Child::Disk(p);
                        ptrs.push((*k, p));
                    }
                    encode_internal(*level, &ptrs, st.txn, &mut buf);
                }
            }
            st.write_new(&buf)?
        }
    };
    *c = Child::Disk(ptr);
    Ok(ptr)
}

/// Walk a committed tree. `node(ptr)` is called for every node pointer and returns whether
/// to descend into it; `item(key, value)` for every leaf item reached.
pub fn walk(
    st: &Store,
    p: BlockPtr,
    node: &mut dyn FnMut(&BlockPtr) -> bool,
    item: &mut dyn FnMut(&Key, &[u8]) -> Result<()>,
) -> Result<()> {
    if p.is_none() || !node(&p) {
        return Ok(());
    }
    match load(st, p)? {
        Node::Leaf(items) => {
            for (k, v) in &items {
                item(k, v)?;
            }
        }
        Node::Internal(_, kids) => {
            for (_, c) in kids {
                if let Child::Disk(cp) = c {
                    walk(st, cp, node, item)?;
                }
            }
        }
    }
    Ok(())
}

/// The data pointer stored in a `K_EXTENT` item value.
pub fn extent_ptr(k: &Key, v: &[u8]) -> Option<BlockPtr> {
    (k.kind == K_EXTENT && v.len() >= PTR_SIZE).then(|| BlockPtr::get(v))
}
