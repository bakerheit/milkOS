//! The tab-separated line format both data files use. Fields are escaped so a title or note
//! can hold tabs, newlines and backslashes.

use alloc::string::String;

pub fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '\t' => o.push_str("\\t"),
            '\n' => o.push_str("\\n"),
            '\r' => {}
            c => o.push(c),
        }
    }
    o
}

pub fn unesc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            o.push(c);
            continue;
        }
        match it.next() {
            Some('t') => o.push('\t'),
            Some('n') => o.push('\n'),
            Some('\\') => o.push('\\'),
            Some(x) => {
                o.push('\\');
                o.push(x);
            }
            None => o.push('\\'),
        }
    }
    o
}

/// "HH:MM" for minutes since midnight.
pub fn fmt_hm(min: u16) -> String {
    alloc::format!("{:02}:{:02}", min / 60, min % 60)
}

/// "HH:MM" to minutes since midnight (0:00 to 23:59).
pub fn parse_hm(s: &str) -> Option<u16> {
    let (h, m) = s.split_once(':')?;
    let (h, m): (u16, u16) = (h.trim().parse().ok()?, m.trim().parse().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_roundtrip() {
        let s = "a\tb\nc\\d\\n";
        assert_eq!(unesc(&esc(s)), s);
        assert!(!esc(s).contains('\t') && !esc(s).contains('\n'));
    }

    #[test]
    fn hm() {
        assert_eq!(fmt_hm(0), "00:00");
        assert_eq!(fmt_hm(23 * 60 + 59), "23:59");
        assert_eq!(parse_hm("07:05"), Some(425));
        assert_eq!(parse_hm("7:5"), Some(425));
        assert_eq!(parse_hm("24:00"), None);
        assert_eq!(parse_hm("12:60"), None);
        assert_eq!(parse_hm("noon"), None);
    }
}
