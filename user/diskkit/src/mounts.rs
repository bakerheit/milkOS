//! `/proc/mounts`: which disks there are.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// A milkfs volume (the Milk disk, or a clone mounted beside it).
    Milk,
    /// A FAT32 volume.
    Fat,
    /// The folder shared with the Mac.
    MacShare,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Volume {
    pub name: String,
    pub path: String,
    pub kind: Kind,
}

impl Volume {
    pub fn kind_label(&self) -> &'static str {
        match self.kind {
            Kind::Milk => "milkfs",
            Kind::Fat => "FAT32",
            Kind::MacShare => "Shared with the Mac",
        }
    }
}

/// The disks under `/Volumes`: milkfs, FAT and the Mac share. Clones (`Milk-name`) are included;
/// the caller drops the ones its clone list names.
pub fn volumes(mounts: &str) -> Vec<Volume> {
    mounts
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let (fs, path) = (it.next()?, it.next()?);
            let name = path.strip_prefix("/Volumes/")?;
            if name.is_empty() || name.contains('/') {
                return None;
            }
            let kind = match fs {
                "milkfs" => Kind::Milk,
                "vfat" | "fat" | "fat32" => Kind::Fat,
                "9p" => Kind::MacShare,
                _ => return None,
            };
            Some(Volume {
                name: name.to_string(),
                path: path.to_string(),
                kind,
            })
        })
        .collect()
}

/// Is this mount one of the clones (a milkfs volume named in the clone list's mount points)?
pub fn is_clone_mount(v: &Volume, clone_mounts: &[Option<String>]) -> bool {
    v.kind == Kind::Milk && clone_mounts.iter().flatten().any(|m| *m == v.path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_disks() {
        let t = "tmpfs / tmpfs rw 0 0\ndevfs /dev devfs rw 0 0\nmilkfs /Volumes/Milk milkfs rw 0 0\nmilkfs /home/milk milkfs rw 0 0\nvfat /Volumes/USB vfat rw 0 0\n9p /Volumes/Mac 9p rw 0 0\nmilkfs /Volumes/Milk-try1 milkfs rw 0 0\nproc /proc proc rw 0 0\n";
        let v = volumes(t);
        let names: Vec<(&str, Kind)> = v.iter().map(|x| (x.name.as_str(), x.kind)).collect();
        assert_eq!(
            names,
            [
                ("Milk", Kind::Milk),
                ("USB", Kind::Fat),
                ("Mac", Kind::MacShare),
                ("Milk-try1", Kind::Milk)
            ]
        );
        assert_eq!(v[2].kind_label(), "Shared with the Mac");
        let clones = [Some("/Volumes/Milk-try1".to_string()), None];
        assert!(is_clone_mount(&v[3], &clones));
        assert!(!is_clone_mount(&v[0], &clones));
    }
}
