//! Table commands for structured pipes (docs/shell.md): where, sort-by, select, first,
//! last, get, count, table, to-json. Each runs as a pipeline stage (in a forked child):
//! read stdin as a table (or columnar text), transform, write.

use alloc::string::String;
use alloc::vec::Vec;
use rt::io::Out;
use rt::table::{Table, Ty, Val, compare, parse_size};
use rt::{eprintln, sys};

pub fn is_table_cmd(name: &str) -> bool {
    matches!(
        name,
        "where" | "sort-by" | "select" | "first" | "last" | "get" | "count" | "table" | "to-json"
    )
}

/// Commands whose output is a table (the rest print text).
pub fn emits_table(name: &str) -> bool {
    matches!(name, "where" | "sort-by" | "select" | "first" | "last")
}

fn read_stdin() -> Vec<u8> {
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match sys::read(0, &mut buf) {
            Ok(0) => break,
            Ok(n) => data.extend_from_slice(&buf[..n]),
            Err(e) if e.0 == 4 => continue,
            Err(_) => break,
        }
    }
    data
}

fn column(t: &Table, cmd: &str, name: &str) -> Result<usize, i32> {
    t.col(name).ok_or_else(|| {
        let names: Vec<&str> = t.cols.iter().map(|c| c.0.as_str()).collect();
        eprintln!("{cmd}: no column '{name}' (columns: {})", names.join(" "));
        1
    })
}

fn literal(ty: Ty, s: &str) -> Val {
    match ty {
        Ty::Size => parse_size(s).map_or_else(|| Val::str(s), Val::Int),
        Ty::Int => s
            .parse()
            .ok()
            .or_else(|| parse_size(s))
            .map_or_else(|| Val::str(s), Val::Int),
        Ty::Str | Ty::Ver => Val::str(s),
    }
}

fn op_of(rest: &str) -> &str {
    let end = rest
        .char_indices()
        .find(|&(_, c)| !"<>=!~ ".contains(c))
        .map_or(rest.len(), |(i, _)| i);
    rest[..end].trim()
}

fn where_(t: &mut Table, args: &[String]) -> Result<(), i32> {
    if args.len() < 2 {
        eprintln!("usage: where COLUMN OP VALUE   (OP: == != < <= > >= =~ !~)");
        return Err(2);
    }
    let c = column(t, "where", &args[0])?;
    let ty = t.cols[c].1;
    // The operator may arrive split ("> = 100" from `>=`, since the lexer cuts at '>').
    let rest = args[1..].join(" ");
    let mut op = String::new();
    let mut chars = rest.char_indices().peekable();
    let mut value_start = rest.len();
    while let Some(&(i, ch)) = chars.peek() {
        if "<>=!~".contains(ch) {
            op.push(ch);
        } else if ch != ' ' {
            value_start = i;
            break;
        }
        chars.next();
    }
    let op = op.as_str();
    let raw = String::from(rest[value_start..].trim());
    let lit = literal(ty, &raw);
    if ty.numeric() && !matches!(lit, Val::Int(_)) && !matches!(op_of(&rest), "=~" | "!~") {
        eprintln!(
            "where: column '{}' is a number; '{raw}' isn't (sizes may use K, M, G)",
            args[0]
        );
        return Err(2);
    }
    let keep = |v: &Val| -> bool {
        use core::cmp::Ordering::*;
        match op {
            "=~" => v.text().contains(raw.as_str()),
            "!~" => !v.text().contains(raw.as_str()),
            _ => {
                let o = compare(v, &lit, ty);
                match op {
                    "==" | "=" => o == Equal,
                    "!=" => o != Equal,
                    "<" => o == Less,
                    "<=" => o != Greater,
                    ">" => o == Greater,
                    ">=" => o != Less,
                    _ => false,
                }
            }
        }
    };
    if !matches!(
        op,
        "==" | "=" | "!=" | "<" | "<=" | ">" | ">=" | "=~" | "!~"
    ) {
        eprintln!("where: unknown operator '{op}' (== != < <= > >= =~ !~)");
        return Err(2);
    }
    t.rows.retain(|r| keep(r.get(c).unwrap_or(&Val::Nil)));
    Ok(())
}

pub fn run(argv: &[String]) -> i32 {
    let data = read_stdin();
    let mut t = Table::parse(&data);
    let cmd = argv[0].as_str();
    let args = &argv[1..];
    let mut out = Out::stdout();
    let r: Result<(), i32> = (|| {
        match cmd {
            "where" => where_(&mut t, args)?,
            "sort-by" => {
                let Some(name) = args.iter().find(|a| !a.starts_with('-')) else {
                    eprintln!("usage: sort-by COLUMN [-r]");
                    return Err(2);
                };
                let c = column(&t, cmd, name)?;
                let ty = t.cols[c].1;
                t.rows.sort_by(|a, b| {
                    compare(
                        a.get(c).unwrap_or(&Val::Nil),
                        b.get(c).unwrap_or(&Val::Nil),
                        ty,
                    )
                });
                if args.iter().any(|a| a == "-r" || a == "--reverse") {
                    t.rows.reverse();
                }
            }
            "select" => {
                let idx: Vec<usize> = args
                    .iter()
                    .map(|n| column(&t, cmd, n))
                    .collect::<Result<_, _>>()?;
                t.cols = idx.iter().map(|&i| t.cols[i].clone()).collect();
                t.rows = t
                    .rows
                    .iter()
                    .map(|r| {
                        idx.iter()
                            .map(|&i| r.get(i).cloned().unwrap_or(Val::Nil))
                            .collect()
                    })
                    .collect();
            }
            "first" | "last" => {
                let n: usize = args.first().and_then(|s| s.parse().ok()).unwrap_or(1);
                if cmd == "first" {
                    t.rows.truncate(n);
                } else {
                    let skip = t.rows.len().saturating_sub(n);
                    t.rows.drain(..skip);
                }
            }
            "get" => {
                let Some(name) = args.first() else {
                    eprintln!("usage: get COLUMN");
                    return Err(2);
                };
                let c = column(&t, cmd, name)?;
                for r in &t.rows {
                    out.line(&r.get(c).unwrap_or(&Val::Nil).text());
                }
                return Ok(());
            }
            "count" => {
                out.line(&alloc::format!("{}", t.rows.len()));
                return Ok(());
            }
            "table" => {
                t.render(&mut out);
                return Ok(());
            }
            "to-json" => {
                t.to_json(&mut out);
                return Ok(());
            }
            _ => return Err(127),
        }
        t.write(&mut out);
        Ok(())
    })();
    out.flush();
    match r {
        Ok(()) => 0,
        Err(c) => c,
    }
}
