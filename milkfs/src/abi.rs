//! The pure parts of the file metadata ABI milkfs stores and the kernel's VFS speaks: file
//! types (`S_IF*`, `DT_*`, [`InodeType`]) and [`Timespec`]. The kernel re-exports them from
//! `fs::node`.

/// File type, as in the `S_IFMT` bits of `st_mode`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InodeType {
    File,
    Dir,
    Symlink,
    CharDev,
    BlockDev,
    Fifo,
    Socket,
}

pub const S_IFMT: u32 = 0o170000;
pub const S_IFSOCK: u32 = 0o140000;
pub const S_IFLNK: u32 = 0o120000;
pub const S_IFREG: u32 = 0o100000;
pub const S_IFBLK: u32 = 0o060000;
pub const S_IFDIR: u32 = 0o040000;
pub const S_IFCHR: u32 = 0o020000;
pub const S_IFIFO: u32 = 0o010000;

/// `linux_dirent64.d_type` values.
pub const DT_UNKNOWN: u8 = 0;
pub const DT_FIFO: u8 = 1;
pub const DT_CHR: u8 = 2;
pub const DT_DIR: u8 = 4;
pub const DT_BLK: u8 = 6;
pub const DT_REG: u8 = 8;
pub const DT_LNK: u8 = 10;
pub const DT_SOCK: u8 = 12;

impl InodeType {
    /// The `S_IF*` bits for `st_mode`.
    pub const fn mode_bits(self) -> u32 {
        match self {
            InodeType::File => S_IFREG,
            InodeType::Dir => S_IFDIR,
            InodeType::Symlink => S_IFLNK,
            InodeType::CharDev => S_IFCHR,
            InodeType::BlockDev => S_IFBLK,
            InodeType::Fifo => S_IFIFO,
            InodeType::Socket => S_IFSOCK,
        }
    }

    pub const fn from_mode(mode: u32) -> Option<InodeType> {
        Some(match mode & S_IFMT {
            S_IFREG => InodeType::File,
            S_IFDIR => InodeType::Dir,
            S_IFLNK => InodeType::Symlink,
            S_IFCHR => InodeType::CharDev,
            S_IFBLK => InodeType::BlockDev,
            S_IFIFO => InodeType::Fifo,
            S_IFSOCK => InodeType::Socket,
            _ => return None,
        })
    }

    /// The `d_type` byte for `linux_dirent64`.
    pub const fn dirent_type(self) -> u8 {
        match self {
            InodeType::File => DT_REG,
            InodeType::Dir => DT_DIR,
            InodeType::Symlink => DT_LNK,
            InodeType::CharDev => DT_CHR,
            InodeType::BlockDev => DT_BLK,
            InodeType::Fifo => DT_FIFO,
            InodeType::Socket => DT_SOCK,
        }
    }

    /// One letter, as in the first column of `ls -l`.
    pub const fn letter(self) -> char {
        match self {
            InodeType::File => '-',
            InodeType::Dir => 'd',
            InodeType::Symlink => 'l',
            InodeType::CharDev => 'c',
            InodeType::BlockDev => 'b',
            InodeType::Fifo => 'p',
            InodeType::Socket => 's',
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Timespec {
    pub sec: i64,
    pub nsec: i64,
}
