//! Linux errno values: [`Errno`] and [`Result`], defined in the milkfs crate (which shares
//! them with the kernel), re-exported here so the rest of the kernel keeps `fs::errno`.

pub use ::milkfs::errno::{Errno, Result};
