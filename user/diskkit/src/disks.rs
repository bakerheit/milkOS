//! `/sys/fs/milkfs/disks`: every block device the system found, one line each:
//!
//! ```text
//! vda 67108864 rw milkfs plain mounted=/Volumes/Milk Milk
//! vdb 33554432 rw milkfs encrypted locked Vault
//! vdc 16777216 rw milkfs plain unmounted Spare disk
//! vdd 67108864 ro other
//! ```
//!
//! The label comes last because it may have spaces.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Mounted(String),
    Locked,
    Unmounted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Content {
    Milkfs {
        encrypted: bool,
        state: State,
        label: String,
    },
    /// A disk that is not milkfs (FAT, blank, ...).
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskEntry {
    pub name: String,
    pub bytes: u64,
    pub read_only: bool,
    pub content: Content,
}

impl DiskEntry {
    pub fn kb(&self) -> u64 {
        self.bytes / 1024
    }

    /// Can a new milkfs volume be made here (the Format flow)? Only a disk that holds no milkfs.
    pub fn can_format(&self) -> bool {
        !self.read_only && matches!(self.content, Content::Other)
    }
}

pub fn parse(text: &str) -> Vec<DiskEntry> {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 4 {
                return None;
            }
            let bytes = f[1].parse().ok()?;
            let read_only = match f[2] {
                "rw" => false,
                "ro" => true,
                _ => return None,
            };
            let content = match f[3] {
                "other" => Content::Other,
                "milkfs" => {
                    let encrypted = match *f.get(4)? {
                        "encrypted" => true,
                        "plain" => false,
                        _ => return None,
                    };
                    let state = match *f.get(5)? {
                        "locked" => State::Locked,
                        "unmounted" => State::Unmounted,
                        m => State::Mounted(m.strip_prefix("mounted=")?.to_string()),
                    };
                    // The label is whatever follows the state, spaces included.
                    let label = f[6..].join(" ");
                    Content::Milkfs {
                        encrypted,
                        state,
                        label,
                    }
                }
                _ => return None,
            };
            Some(DiskEntry {
                name: f[0].to_string(),
                bytes,
                read_only,
                content,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines() {
        let t = "vda 67108864 rw milkfs plain mounted=/Volumes/Milk Milk\nvdb 33554432 rw milkfs encrypted locked Vault\nvdc 16777216 rw milkfs plain unmounted Spare disk\nvdd 67108864 ro other\nvde 1024 rw other\nbroken\nvdf x rw other\nvdg 1 rw milkfs maybe locked X\n";
        let v = parse(t);
        assert_eq!(v.len(), 5);
        assert_eq!(
            v[0].content,
            Content::Milkfs {
                encrypted: false,
                state: State::Mounted("/Volumes/Milk".into()),
                label: "Milk".into()
            }
        );
        assert_eq!(
            v[1].content,
            Content::Milkfs {
                encrypted: true,
                state: State::Locked,
                label: "Vault".into()
            }
        );
        match &v[2].content {
            Content::Milkfs { label, state, .. } => {
                assert_eq!((label.as_str(), state), ("Spare disk", &State::Unmounted))
            }
            c => panic!("{c:?}"),
        }
        assert!(v[3].read_only && v[3].content == Content::Other);
        assert_eq!(v[0].kb(), 65_536);
        assert!(!v[3].can_format() && v[4].can_format() && !v[0].can_format());
    }
}
