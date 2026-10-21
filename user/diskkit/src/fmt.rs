//! Sizes for people.

use alloc::format;
use alloc::string::String;

/// Kilobytes as "512 KB", "63.4 MB" or "1.2 GB".
pub fn kb(n: u64) -> String {
    if n >= 1 << 20 {
        format!("{}.{} GB", n >> 20, ((n & ((1 << 20) - 1)) * 10) >> 20)
    } else if n >= 1 << 10 {
        format!("{}.{} MB", n >> 10, ((n & ((1 << 10) - 1)) * 10) >> 10)
    } else {
        format!("{n} KB")
    }
}

/// Bytes the same way.
pub fn bytes(n: u64) -> String {
    if n < 1024 {
        format!("{n} bytes")
    } else {
        kb(n / 1024)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(kb(512), "512 KB");
        assert_eq!(kb(65_536), "64.0 MB");
        assert_eq!(kb(64_900), "63.3 MB");
        assert_eq!(kb(1_258_291), "1.1 GB");
        assert_eq!(bytes(100), "100 bytes");
        assert_eq!(bytes(22_700_000), "21.6 MB");
    }
}
