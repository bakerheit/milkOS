//! Structured pipes: a table of named, typed columns, its wire format, a text renderer.
//! See docs/shell.md.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::Ordering;
use core::fmt::Write;

use crate::io::Out;

pub const MAGIC: &str = "\x1emilk-table 1";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ty {
    Int,
    /// Bytes; rendered as 1.5K, literals may carry a unit.
    Size,
    Str,
}

impl Ty {
    fn name(self) -> &'static str {
        match self {
            Ty::Int => "int",
            Ty::Size => "size",
            Ty::Str => "str",
        }
    }
    fn parse(s: &str) -> Ty {
        match s {
            "int" => Ty::Int,
            "size" => Ty::Size,
            _ => Ty::Str,
        }
    }
    pub fn numeric(self) -> bool {
        self != Ty::Str
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum Val {
    Int(i64),
    Str(String),
    Nil,
}

impl Val {
    pub fn str(s: &str) -> Val {
        Val::Str(String::from(s))
    }
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Val::Int(i) => Some(*i),
            Val::Str(s) => s.parse().ok(),
            Val::Nil => None,
        }
    }
    /// Plain text (sizes as bytes).
    pub fn text(&self) -> String {
        match self {
            Val::Int(i) => format!("{i}"),
            Val::Str(s) => s.clone(),
            Val::Nil => String::new(),
        }
    }
}

/// Compare two values of a column of type `ty`.
pub fn compare(a: &Val, b: &Val, ty: Ty) -> Ordering {
    if ty.numeric() {
        match (a.as_int(), b.as_int()) {
            (Some(x), Some(y)) => x.cmp(&y),
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        }
    } else {
        a.text().cmp(&b.text())
    }
}

/// "100K" -> 102400, "2M", "1G", "1.5K"; bare numbers are bytes.
pub fn parse_size(s: &str) -> Option<i64> {
    let s = s.trim();
    let (num, mult) = match s.chars().last()?.to_ascii_uppercase() {
        'K' => (&s[..s.len() - 1], 1i64 << 10),
        'M' => (&s[..s.len() - 1], 1 << 20),
        'G' => (&s[..s.len() - 1], 1 << 30),
        'B' => (&s[..s.len() - 1], 1),
        _ => (s, 1),
    };
    if let Ok(v) = num.parse::<i64>() {
        return Some(v * mult);
    }
    let v: f64 = num.parse().ok()?;
    Some((v * mult as f64) as i64)
}

fn human(n: i64) -> String {
    let units = ["B", "K", "M", "G", "T"];
    let mut v = n as f64;
    let mut u = 0;
    while v.abs() >= 1024.0 && u < units.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n}B")
    } else if v < 10.0 {
        format!("{:.1}{}", v, units[u])
    } else {
        format!("{:.0}{}", v, units[u])
    }
}

fn escape(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
}

fn unescape(s: &str) -> String {
    let mut o = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            o.push(c);
            continue;
        }
        match it.next() {
            Some('t') => o.push('\t'),
            Some('n') => o.push('\n'),
            Some(c) => o.push(c),
            None => o.push('\\'),
        }
    }
    o
}

#[derive(Clone, Default)]
pub struct Table {
    pub cols: Vec<(String, Ty)>,
    pub rows: Vec<Vec<Val>>,
}

impl Table {
    pub fn new(cols: &[(&str, Ty)]) -> Table {
        Table {
            cols: cols.iter().map(|(n, t)| (String::from(*n), *t)).collect(),
            rows: Vec::new(),
        }
    }

    pub fn push(&mut self, row: Vec<Val>) {
        self.rows.push(row);
    }

    pub fn col(&self, name: &str) -> Option<usize> {
        self.cols.iter().position(|(n, _)| n == name).or_else(|| {
            // Be forgiving about case: /proc/threads says PRIO, people type prio.
            self.cols
                .iter()
                .position(|(n, _)| n.eq_ignore_ascii_case(name))
        })
    }

    /// The wire format.
    pub fn write(&self, out: &mut Out) {
        let mut s = String::from(MAGIC);
        s.push('\n');
        for (i, (n, t)) in self.cols.iter().enumerate() {
            if i > 0 {
                s.push('\t');
            }
            escape(n, &mut s);
            s.push(':');
            s.push_str(t.name());
        }
        s.push('\n');
        out.write(s.as_bytes());
        for r in &self.rows {
            let mut l = String::new();
            for (i, v) in r.iter().enumerate() {
                if i > 0 {
                    l.push('\t');
                }
                escape(&v.text(), &mut l);
            }
            l.push('\n');
            out.write(l.as_bytes());
        }
    }

    fn cell(&self, c: usize, v: &Val) -> String {
        match (self.cols[c].1, v) {
            (Ty::Size, Val::Int(i)) => human(*i),
            _ => v.text(),
        }
    }

    /// Aligned text with a header row.
    pub fn render(&self, out: &mut Out) {
        if self.cols.is_empty() {
            return; // nothing came in (e.g. the producer failed)
        }
        let mut w: Vec<usize> = self.cols.iter().map(|(n, _)| n.chars().count()).collect();
        let cells: Vec<Vec<String>> = self
            .rows
            .iter()
            .map(|r| {
                (0..self.cols.len())
                    .map(|c| self.cell(c, r.get(c).unwrap_or(&Val::Nil)))
                    .collect()
            })
            .collect();
        for r in &cells {
            for (c, s) in r.iter().enumerate() {
                w[c] = w[c].max(s.chars().count());
            }
        }
        let line = |vals: &[String], header: bool| -> String {
            let mut l = String::new();
            for (c, v) in vals.iter().enumerate() {
                if c > 0 {
                    l.push_str("  ");
                }
                let width = w[c];
                if self.cols[c].1.numeric() && !header {
                    let _ = write!(l, "{v:>width$}");
                } else {
                    let _ = write!(l, "{v:<width$}");
                }
            }
            String::from(l.trim_end())
        };
        let header: Vec<String> = self.cols.iter().map(|(n, _)| n.clone()).collect();
        out.line(&line(&header, true));
        let rule: Vec<String> = w.iter().map(|&n| "-".repeat(n)).collect();
        out.line(&line(&rule, true));
        for r in &cells {
            out.line(&line(r, false));
        }
    }

    pub fn to_json(&self, out: &mut Out) {
        out.write(b"[\n");
        for (i, r) in self.rows.iter().enumerate() {
            let mut l = String::from("  {");
            for (c, (n, t)) in self.cols.iter().enumerate() {
                if c > 0 {
                    l.push_str(", ");
                }
                let v = r.get(c).unwrap_or(&Val::Nil);
                let _ = write!(l, "{}: ", json_str(n));
                match (t.numeric(), v.as_int(), v) {
                    (_, _, Val::Nil) => l.push_str("null"),
                    (true, Some(x), _) => {
                        let _ = write!(l, "{x}");
                    }
                    _ => l.push_str(&json_str(&v.text())),
                }
            }
            l.push('}');
            if i + 1 < self.rows.len() {
                l.push(',');
            }
            out.line(&l);
        }
        out.write(b"]\n");
    }

    /// Parse the wire format, or (fallback) columnar text whose first line is a header.
    pub fn parse(data: &[u8]) -> Table {
        let text = String::from_utf8_lossy(data);
        let mut lines = text.lines();
        let first = lines.clone().next().unwrap_or("");
        if first == MAGIC {
            lines.next();
            let cols: Vec<(String, Ty)> = lines
                .next()
                .unwrap_or("")
                .split('\t')
                .filter(|c| !c.is_empty())
                .map(|c| match c.rsplit_once(':') {
                    Some((n, t)) => (unescape(n), Ty::parse(t)),
                    None => (unescape(c), Ty::Str),
                })
                .collect();
            let rows = lines
                .filter(|l| !l.is_empty())
                .map(|l| {
                    l.split('\t')
                        .enumerate()
                        .map(|(i, f)| {
                            let f = unescape(f);
                            match cols.get(i).map(|c| c.1) {
                                Some(t) if t.numeric() => f.parse().map_or(
                                    if f.is_empty() { Val::Nil } else { Val::Str(f) },
                                    Val::Int,
                                ),
                                _ => Val::Str(f),
                            }
                        })
                        .collect()
                })
                .collect();
            return Table { cols, rows };
        }
        // Text: header + whitespace-separated columns; the last column takes the rest.
        let header: Vec<&str> = first.split_whitespace().collect();
        let n = header.len();
        let mut rows: Vec<Vec<Val>> = Vec::new();
        for l in text.lines().skip(1) {
            if l.trim().is_empty() {
                continue;
            }
            let mut fields: Vec<String> = Vec::new();
            let mut rest = l.trim_start();
            while fields.len() + 1 < n {
                match rest.find(char::is_whitespace) {
                    Some(i) => {
                        fields.push(String::from(&rest[..i]));
                        rest = rest[i..].trim_start();
                    }
                    None => break,
                }
            }
            fields.push(String::from(rest.trim_end()));
            rows.push(fields.into_iter().map(Val::Str).collect());
        }
        // Columns where every value is an integer become int.
        let mut cols = Vec::new();
        for (c, h) in header.iter().enumerate() {
            let numeric = !rows.is_empty()
                && rows
                    .iter()
                    .all(|r| r.get(c).is_none_or(|v| v.text().parse::<i64>().is_ok()));
            cols.push((String::from(*h), if numeric { Ty::Int } else { Ty::Str }));
            if numeric {
                for r in rows.iter_mut() {
                    if let Some(v) = r.get_mut(c) {
                        *v = v.as_int().map_or(Val::Nil, Val::Int);
                    }
                }
            }
        }
        Table { cols, rows }
    }
}

fn json_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// Did msh ask this program for table output? (`MILK_TABLE=1` in the environment.)
pub fn requested(args: &crate::Args) -> bool {
    args.env("MILK_TABLE") == Some("1")
}
