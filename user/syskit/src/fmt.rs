//! Aligned `key  value` lines, the way `files info` prints them.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// One line per pair, the values lined up two spaces after the longest key. Pairs with an
/// empty value are left out.
pub fn pairs(rows: &[(&str, String)]) -> Vec<String> {
    let w = rows
        .iter()
        .filter(|r| !r.1.is_empty())
        .map(|r| r.0.len())
        .max()
        .unwrap_or(0);
    rows.iter()
        .filter(|r| !r.1.is_empty())
        .map(|(k, v)| format!("{k:<w$}  {v}"))
        .collect()
}

/// "yes" / "no".
pub fn yes(b: bool) -> String {
    String::from(if b { "yes" } else { "no" })
}

/// Kilobytes as people read them: "512 KB", "1.5 MB", "24 MB", "1.2 GB".
pub fn kb(n: u64) -> String {
    filekit::words::size(n.saturating_mul(1024))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligned() {
        let v = pairs(&[
            ("name", String::from("milk")),
            ("skipped", String::new()),
            ("groups", String::from("milk, admin")),
        ]);
        assert_eq!(v, ["name    milk", "groups  milk, admin"]);
        assert!(pairs(&[]).is_empty());
        assert_eq!(yes(true), "yes");
        assert_eq!(kb(512), "512 KB");
        assert_eq!(kb(1536), "1.5 MB");
        assert_eq!(kb(24 * 1024), "24 MB");
    }
}
