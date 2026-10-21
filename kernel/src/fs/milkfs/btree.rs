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
//! when their encoding outgrows a block. On deletion a node left under a quarter full is
//! merged with a sibling when both fit in one block, otherwise the two are rebalanced
//! (split evenly again); a root left with one child is replaced by it.

use alloc::boxed::Box;
use alloc::vec::Vec;

use super::format::{
    BLOCK, BlockPtr, DiskNode, INTERNAL_ENTRY, Key, LEAF_ITEM_OVERHEAD, MAX_VALUE, NODE_HEADER,
    decode_node, encode_internal, encode_leaf,
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

/// An owned, modifiable copy of the node at `p` (for copy-on-write).
pub fn load(st: &Store, p: BlockPtr) -> Result<Node> {
    Ok(match &*st.node(p)? {
        DiskNode::Leaf(items) => Node::Leaf(items.clone()),
        DiskNode::Internal(level, kids) => Node::Internal(
            *level,
            kids.iter().map(|(k, p)| (*k, Child::Disk(*p))).collect(),
        ),
    })
}

/// Read and decode the node at `p` (checksum verified). Used by `Store::node`'s cache.
pub fn decode_at(st: &Store, p: BlockPtr) -> Result<DiskNode> {
    let mut buf = [0u8; BLOCK];
    st.read(p, &mut buf)?;
    decode_node(&buf)
}

fn leaf_size(items: &[(Key, Vec<u8>)]) -> usize {
    NODE_HEADER
        + items
            .iter()
            .map(|(_, v)| LEAF_ITEM_OVERHEAD + v.len())
            .sum::<usize>()
}

/// Nodes (other than the root) below this many encoded bytes are underfull.
pub const MIN_FILL: usize = BLOCK / 4;

fn internal_fits(n: usize) -> bool {
    NODE_HEADER + n * INTERNAL_ENTRY <= BLOCK
}

/// Index of the child whose range contains `key`.
fn child_index<T>(kids: &[(Key, T)], key: &Key) -> usize {
    kids.partition_point(|(k, _)| k <= key).saturating_sub(1)
}

// ---- reads --------------------------------------------------------------------------

fn leaf_get(items: &[(Key, Vec<u8>)], key: &Key) -> Option<Vec<u8>> {
    items
        .binary_search_by(|(k, _)| k.cmp(key))
        .ok()
        .map(|i| items[i].1.clone())
}

fn leaf_range(items: &[(Key, Vec<u8>)], lo: &Key, hi: &Key, out: &mut Vec<(Key, Vec<u8>)>) {
    let start = items.partition_point(|(k, _)| k < lo);
    for (k, v) in &items[start..] {
        if k >= hi {
            break;
        }
        out.push((*k, v.clone()));
    }
}

pub fn get(c: &Child, st: &Store, key: &Key) -> Result<Option<Vec<u8>>> {
    match c {
        Child::Mem(n) => match &**n {
            Node::Leaf(items) => Ok(leaf_get(items, key)),
            Node::Internal(_, kids) if kids.is_empty() => Ok(None),
            Node::Internal(_, kids) => get(&kids[child_index(kids, key)].1, st, key),
        },
        Child::Disk(p) => match &*st.node(*p)? {
            DiskNode::Leaf(items) => Ok(leaf_get(items, key)),
            DiskNode::Internal(_, kids) if kids.is_empty() => Ok(None),
            DiskNode::Internal(_, kids) => {
                get(&Child::Disk(kids[child_index(kids, key)].1), st, key)
            }
        },
    }
}

/// Every item with `lo <= key < hi`, in key order.
pub fn range(
    c: &Child,
    st: &Store,
    lo: &Key,
    hi: &Key,
    out: &mut Vec<(Key, Vec<u8>)>,
) -> Result<()> {
    match c {
        Child::Mem(n) => match &**n {
            Node::Leaf(items) => leaf_range(items, lo, hi, out),
            Node::Internal(_, kids) if !kids.is_empty() => {
                let first = child_index(kids, lo);
                for (i, (k, child)) in kids.iter().enumerate().skip(first) {
                    if i > first && *k >= *hi {
                        break;
                    }
                    range(child, st, lo, hi, out)?;
                }
            }
            Node::Internal(..) => {}
        },
        Child::Disk(p) => match &*st.node(*p)? {
            DiskNode::Leaf(items) => leaf_range(items, lo, hi, out),
            DiskNode::Internal(_, kids) if !kids.is_empty() => {
                let first = child_index(kids, lo);
                for (i, (k, cp)) in kids.iter().enumerate().skip(first) {
                    if i > first && *k >= *hi {
                        break;
                    }
                    range(&Child::Disk(*cp), st, lo, hi, out)?;
                }
            }
            DiskNode::Internal(..) => {}
        },
    }
    Ok(())
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
            let new_obj = match items.binary_search_by(|(k, _)| k.cmp(&key)) {
                Ok(i) => {
                    items[i].1 = val;
                    None
                }
                Err(i) => {
                    items.insert(i, (key, val));
                    // The first item of an object newer than every other in this leaf.
                    (i > 0 && items[i - 1].0.obj < key.obj).then_some(i)
                }
            };
            if leaf_size(items) > BLOCK {
                if let Some(i) = new_obj {
                    // Inode numbers only grow, so this is how files get created: split at
                    // the object boundary and leave the left leaf full instead of half.
                    let right = items.split_off(i);
                    if leaf_size(items) <= BLOCK && leaf_size(&right) <= BLOCK {
                        return Ok(Some((right[0].0, Child::Mem(Box::new(Node::Leaf(right))))));
                    }
                    items.extend(right);
                }
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
            let size = match &kids[idx].1 {
                Child::Mem(n) => Some(node_bytes(n)),
                Child::Disk(_) => None,
            };
            if size == Some(NODE_HEADER) {
                kids.remove(idx); // empty; dirty, so its old block was already released
            } else if size.is_some_and(|s| s < MIN_FILL) && kids.len() > 1 {
                rebalance(kids, idx, st)?;
            }
            Ok(found)
        }
    }
}

/// Encoded size of a node.
fn node_bytes(n: &Node) -> usize {
    match n {
        Node::Leaf(items) => leaf_size(items),
        Node::Internal(_, kids) => NODE_HEADER + kids.len() * INTERNAL_ENTRY,
    }
}

/// `kids[idx]` is underfull: merge it with a neighbour if the two fit in one block,
/// otherwise share their entries evenly between the two.
fn rebalance(kids: &mut Vec<(Key, Child)>, idx: usize, st: &mut Store) -> Result<()> {
    let (l, r) = if idx + 1 < kids.len() {
        (idx, idx + 1)
    } else {
        (idx - 1, idx)
    };
    make_mut(&mut kids[l].1, st)?;
    make_mut(&mut kids[r].1, st)?;
    let (rsep, right) = kids.remove(r);
    let Child::Mem(right) = right else {
        unreachable!()
    };
    let Child::Mem(left) = &mut kids[l].1 else {
        unreachable!()
    };
    match (&mut **left, *right) {
        (Node::Leaf(a), Node::Leaf(b)) => {
            a.extend(b);
            if leaf_size(a) > BLOCK {
                let split = split_leaf(a);
                kids.insert(r, split);
            }
        }
        (Node::Internal(la, a), Node::Internal(lb, mut b)) if *la == lb => {
            // The right node's first separator was implicit: it is the parent's.
            if let Some(first) = b.first_mut() {
                first.0 = rsep;
            }
            a.extend(b);
            if !internal_fits(a.len()) {
                let right = a.split_off(a.len() / 2);
                let k = right[0].0;
                let level = *la;
                kids.insert(r, (k, Child::Mem(Box::new(Node::Internal(level, right)))));
            }
        }
        (_, other) => {
            // Siblings at different levels can't happen (all leaves are at one depth);
            // leave the tree as it was rather than lose entries.
            kids.insert(r, (rsep, Child::Mem(Box::new(other))));
            return Err(Errno::EIO);
        }
    }
    Ok(())
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
    match &*st.node(p)? {
        DiskNode::Leaf(items) => {
            for (k, v) in items {
                item(k, v)?;
            }
        }
        DiskNode::Internal(_, kids) => {
            for (_, cp) in kids {
                walk(st, *cp, node, item)?;
            }
        }
    }
    Ok(())
}

/// Shape of a committed tree (for tests and `/sys/fs/milkfs/treetest`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TreeStats {
    /// Levels (a lone root leaf = 1).
    pub height: u32,
    pub nodes: u64,
    pub leaves: u64,
    pub items: u64,
    /// Encoded bytes of all nodes (fill = bytes / (nodes * BLOCK)).
    pub bytes: u64,
    /// Nodes other than the root below a quarter full.
    pub underfull: u64,
}

impl TreeStats {
    pub fn fill_pct(&self) -> u64 {
        if self.nodes == 0 {
            0
        } else {
            self.bytes * 100 / (self.nodes * BLOCK as u64)
        }
    }
}

pub fn stats(st: &Store, root: BlockPtr) -> Result<TreeStats> {
    fn rec(st: &Store, p: BlockPtr, depth: u32, s: &mut TreeStats) -> Result<()> {
        s.nodes += 1;
        s.height = s.height.max(depth);
        let size = match &*st.node(p)? {
            DiskNode::Leaf(items) => {
                s.leaves += 1;
                s.items += items.len() as u64;
                leaf_size(items)
            }
            DiskNode::Internal(_, kids) => {
                for (_, c) in kids {
                    rec(st, *c, depth + 1, s)?;
                }
                NODE_HEADER + kids.len() * INTERNAL_ENTRY
            }
        };
        s.bytes += size as u64;
        if depth > 1 && size < MIN_FILL {
            s.underfull += 1;
        }
        Ok(())
    }
    let mut s = TreeStats::default();
    if !root.is_none() {
        rec(st, root, 1, &mut s)?;
    }
    Ok(s)
}
