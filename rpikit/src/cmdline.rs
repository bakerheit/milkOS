//! `milk.<name>=<value>` options on the kernel command line.
//!
//! The Raspberry Pi has no qemu fw_cfg; the options the tests and the launcher pass as
//! `opt/milkos/<name>` files travel on the command line instead (cmdline.txt on the SD card,
//! `-append` under qemu). A value with spaces is double-quoted:
//! `milk.net="internet=on dns=1.2.3.4:53"`. Other words on the line (console=, root=, ...)
//! are not ours and are skipped. A word is cut at a space, a newline or a NUL (the tree's
//! string ends in one; cmdline.txt ends in a newline).

/// The option `milk.<name>`: `(offset, length)` of its value inside `line`, without the
/// quotes of a quoted value. `None` if the option is absent. The first of two equal names
/// wins.
pub fn find(line: &[u8], name: &str) -> Option<(usize, usize)> {
    let is_sep = |b: u8| b == b' ' || b == b'\n' || b == b'\r' || b == b'\t';
    let mut i = 0;
    while i < line.len() {
        while i < line.len() && (is_sep(line[i]) || line[i] == 0) {
            i += 1;
        }
        if i >= line.len() {
            break;
        }
        let start = i;
        let mut quoted = false;
        while i < line.len() && line[i] != 0 && (quoted || !is_sep(line[i])) {
            if line[i] == b'"' {
                quoted = !quoted;
            }
            i += 1;
        }
        let word = &line[start..i];
        let Some(rest) = word.strip_prefix(b"milk.") else {
            continue;
        };
        let Some(eq) = rest.iter().position(|&c| c == b'=') else {
            continue;
        };
        if &rest[..eq] != name.as_bytes() {
            continue;
        }
        let mut off = start + 5 + eq + 1;
        let mut len = word.len() - (5 + eq + 1);
        if len >= 2 && line[off] == b'"' && line[off + len - 1] == b'"' {
            off += 1;
            len -= 2;
        }
        return Some((off, len));
    }
    None
}

/// The value of option `milk.<name>` as bytes.
pub fn value<'a>(line: &'a [u8], name: &str) -> Option<&'a [u8]> {
    find(line, name).map(|(o, l)| &line[o..o + l])
}

/// The fw_cfg file name a launcher would use (`opt/milkos/selftest`) to the option name
/// (`selftest`). Other files do not exist on the Pi.
pub fn option_name(file: &str) -> Option<&str> {
    file.strip_prefix("opt/milkos/").filter(|n| !n.is_empty())
}
