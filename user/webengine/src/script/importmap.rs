//! Import maps and module specifier resolution (HTML §8.1.? "Import maps", "resolve a module
//! specifier"). No engine in here: a JSON reader bounded for hostile input, the parse that
//! gives an [`ImportMap`] (plus the warnings the console shows), and the resolution of a module
//! specifier against a referrer's address.
//!
//! What is supported: `imports` and `scopes` as the standard has them (URL-like keys are
//! normalized, a trailing `/` makes a prefix mapping, `null` blocks a specifier, longer
//! prefixes win, the first matching scope wins); `integrity` is checked for shape and
//! otherwise ignored. External import maps (`src`) are not a thing browsers load, and only the
//! first import map of a page counts (the caller enforces both).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::net::{Url, parse_url};

/// The largest import map text taken (bytes).
pub const MAX_MAP_BYTES: usize = 1 << 20;
/// Entries (keys) over all of its tables.
pub const MAX_ENTRIES: usize = 20_000;
const MAX_JSON_DEPTH: usize = 24;

// ---- a small, bounded JSON reader --------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    /// In first-appearance order; a repeated key keeps its first place and its last value, as
    /// `JSON.parse` does.
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(v) => v.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
}

struct Reader<'a> {
    s: &'a [u8],
    at: usize,
    nodes: usize,
}

/// Parse a JSON text; the error is a short reason. Nesting deeper than a small bound, or a
/// text with more than [`MAX_ENTRIES`] values, is refused.
pub fn parse_json(text: &str) -> Result<Json, &'static str> {
    if text.len() > MAX_MAP_BYTES {
        return Err("it is too big");
    }
    let mut r = Reader { s: text.as_bytes(), at: 0, nodes: 0 };
    r.ws();
    let v = r.value(0)?;
    r.ws();
    if r.at != r.s.len() {
        return Err("unexpected text after the value");
    }
    Ok(v)
}

impl Reader<'_> {
    fn ws(&mut self) {
        while self.at < self.s.len() && matches!(self.s[self.at], b' ' | b'\t' | b'\n' | b'\r') {
            self.at += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.at).copied()
    }

    fn lit(&mut self, word: &str) -> bool {
        if self.s[self.at..].starts_with(word.as_bytes()) {
            self.at += word.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, &'static str> {
        if depth > MAX_JSON_DEPTH {
            return Err("it is nested too deeply");
        }
        self.nodes += 1;
        if self.nodes > MAX_ENTRIES * 2 {
            return Err("it has too many values");
        }
        match self.peek() {
            None => Err("unexpected end of text"),
            Some(b'{') => {
                self.at += 1;
                let mut out: Vec<(String, Json)> = Vec::new();
                self.ws();
                if self.peek() == Some(b'}') {
                    self.at += 1;
                    return Ok(Json::Obj(out));
                }
                loop {
                    self.ws();
                    if self.peek() != Some(b'"') {
                        return Err("expected a string key");
                    }
                    let k = self.string()?;
                    self.ws();
                    if self.peek() != Some(b':') {
                        return Err("expected ':'");
                    }
                    self.at += 1;
                    self.ws();
                    let v = self.value(depth + 1)?;
                    match out.iter_mut().find(|(ek, _)| *ek == k) {
                        Some(e) => e.1 = v,
                        None => out.push((k, v)),
                    }
                    self.ws();
                    match self.peek() {
                        Some(b',') => self.at += 1,
                        Some(b'}') => {
                            self.at += 1;
                            return Ok(Json::Obj(out));
                        }
                        _ => return Err("expected ',' or '}'"),
                    }
                }
            }
            Some(b'[') => {
                self.at += 1;
                let mut out = Vec::new();
                self.ws();
                if self.peek() == Some(b']') {
                    self.at += 1;
                    return Ok(Json::Arr(out));
                }
                loop {
                    self.ws();
                    out.push(self.value(depth + 1)?);
                    self.ws();
                    match self.peek() {
                        Some(b',') => self.at += 1,
                        Some(b']') => {
                            self.at += 1;
                            return Ok(Json::Arr(out));
                        }
                        _ => return Err("expected ',' or ']'"),
                    }
                }
            }
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(b't') if self.lit("true") => Ok(Json::Bool(true)),
            Some(b'f') if self.lit("false") => Ok(Json::Bool(false)),
            Some(b'n') if self.lit("null") => Ok(Json::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err("unexpected character"),
        }
    }

    fn number(&mut self) -> Result<Json, &'static str> {
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        let digits = |r: &mut Reader| {
            let s = r.at;
            while matches!(r.peek(), Some(b'0'..=b'9')) {
                r.at += 1;
            }
            r.at - s
        };
        match self.peek() {
            Some(b'0') => self.at += 1,
            Some(b'1'..=b'9') => {
                digits(self);
            }
            _ => return Err("bad number"),
        }
        if self.peek() == Some(b'.') {
            self.at += 1;
            if digits(self) == 0 {
                return Err("bad number");
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            if digits(self) == 0 {
                return Err("bad number");
            }
        }
        let text = core::str::from_utf8(&self.s[start..self.at]).map_err(|_| "bad number")?;
        Ok(Json::Num(parse_f64(text)))
    }

    fn hex4(&mut self) -> Result<u32, &'static str> {
        if self.at + 4 > self.s.len() {
            return Err("bad \\u escape");
        }
        let mut v = 0u32;
        for i in 0..4 {
            let c = self.s[self.at + i];
            let d = match c {
                b'0'..=b'9' => c - b'0',
                b'a'..=b'f' => c - b'a' + 10,
                b'A'..=b'F' => c - b'A' + 10,
                _ => return Err("bad \\u escape"),
            };
            v = v * 16 + d as u32;
        }
        self.at += 4;
        Ok(v)
    }

    fn string(&mut self) -> Result<String, &'static str> {
        self.at += 1; // the opening quote
        let mut out = String::new();
        loop {
            let start = self.at;
            while self.at < self.s.len() && !matches!(self.s[self.at], b'"' | b'\\') && self.s[self.at] >= 0x20 {
                self.at += 1;
            }
            // The input is a &str, so this slice is valid UTF-8 (it breaks at ASCII bytes).
            out.push_str(core::str::from_utf8(&self.s[start..self.at]).map_err(|_| "bad text")?);
            if out.len() > MAX_MAP_BYTES {
                return Err("a string is too long");
            }
            match self.peek() {
                None => return Err("unterminated string"),
                Some(b'"') => {
                    self.at += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.at += 1;
                    let Some(c) = self.peek() else { return Err("unterminated string") };
                    self.at += 1;
                    match c {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hi = self.hex4()?;
                            let ch = if (0xD800..0xDC00).contains(&hi)
                                && self.s[self.at..].starts_with(b"\\u")
                            {
                                let save = self.at;
                                self.at += 2;
                                let lo = self.hex4()?;
                                if (0xDC00..0xE000).contains(&lo) {
                                    char::from_u32(0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00))
                                } else {
                                    self.at = save;
                                    None
                                }
                            } else {
                                char::from_u32(hi)
                            };
                            out.push(ch.unwrap_or('\u{FFFD}'));
                        }
                        _ => return Err("bad escape"),
                    }
                }
                Some(_) => return Err("a control character in a string"),
            }
        }
    }
}

/// A decimal number's value (only its being a number matters to import maps).
fn parse_f64(text: &str) -> f64 {
    let (mant, exp) = match text.find(['e', 'E']) {
        Some(i) => (&text[..i], text[i + 1..].parse::<i32>().unwrap_or(0)),
        None => (text, 0),
    };
    let neg = mant.starts_with('-');
    let mant = mant.trim_start_matches('-');
    let (int, frac) = mant.split_once('.').unwrap_or((mant, ""));
    let mut v = 0.0f64;
    for c in int.bytes().chain(frac.bytes()) {
        v = v * 10.0 + (c - b'0') as f64;
    }
    let mut e = exp - frac.len() as i32;
    while e > 0 && v.is_finite() {
        v *= 10.0;
        e -= 1;
    }
    while e < 0 && v != 0.0 {
        v /= 10.0;
        e += 1;
    }
    if neg { -v } else { v }
}

// ---- the map -----------------------------------------------------------------------------------

/// One table: normalized keys with their address (None: the specifier is blocked), longest
/// (descending) first.
pub type Table = Vec<(String, Option<String>)>;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImportMap {
    pub imports: Table,
    /// Scope prefix (a URL) and its table, in descending order of prefix.
    pub scopes: Vec<(String, Table)>,
}

impl ImportMap {
    pub fn is_empty(&self) -> bool {
        self.imports.is_empty() && self.scopes.is_empty()
    }
}

/// Why a specifier did not resolve (a TypeError for the page).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolveError(pub String);

fn is_special(scheme: &str) -> bool {
    matches!(scheme, "http" | "https" | "ws" | "wss" | "ftp" | "file")
}

/// "Resolve a URL-like module specifier": a path starting `/`, `./` or `../` against `base`, or
/// an absolute URL.
fn url_like(spec: &str, base: &str) -> Option<Url> {
    if spec.starts_with('/') || spec.starts_with("./") || spec.starts_with("../") {
        parse_url(spec, Some(base))
    } else {
        parse_url(spec, None)
    }
}

/// Descending code unit order. For our purposes (BMP-complete UTF-8 vs UTF-16) byte order of
/// UTF-8 equals code point order, which differs from UTF-16 code unit order only for
/// supplementary characters against U+E000..U+FFFF; map keys with those are not worth a
/// special case.
fn sort_desc<T>(v: &mut [(String, T)]) {
    v.sort_by(|a, b| b.0.cmp(&a.0));
}

fn normalize_table(
    json: &Json,
    base: &str,
    warn: &mut Vec<String>,
    budget: &mut usize,
) -> Result<Table, String> {
    let Json::Obj(entries) = json else {
        return Err(String::from("a table of specifiers must be a JSON object"));
    };
    let mut out: Table = Vec::new();
    for (key, value) in entries {
        if *budget == 0 {
            warn.push(String::from("The import map has too many entries; the rest were ignored."));
            break;
        }
        *budget -= 1;
        if key.is_empty() {
            warn.push(String::from("Invalid empty string specifier in the import map."));
            continue;
        }
        let nkey = match url_like(key, base) {
            Some(u) => u.href(),
            None => key.clone(),
        };
        let Json::Str(addr) = value else {
            warn.push(format!("Invalid address for the specifier \"{key}\" in the import map: it must be a string."));
            out.push((nkey, None));
            continue;
        };
        let Some(url) = url_like(addr, base) else {
            warn.push(format!("Invalid address \"{addr}\" for the specifier \"{key}\" in the import map."));
            out.push((nkey, None));
            continue;
        };
        let href = url.href();
        if key.ends_with('/') && !href.ends_with('/') {
            warn.push(format!(
                "The address \"{addr}\" for the specifier \"{key}\" must end with \"/\" (the specifier does)."
            ));
            out.push((nkey, None));
            continue;
        }
        // A repeated normalized key: the later one wins, in the first one's place.
        match out.iter_mut().find(|(k, _)| *k == nkey) {
            Some(e) => e.1 = Some(href),
            None => out.push((nkey, Some(href))),
        }
    }
    sort_desc(&mut out);
    Ok(out)
}

/// Parse an import map's text. `base` is the address of the document (inline import maps
/// resolve against it). `Err` is the TypeError's message (the map is then not used); the
/// warnings of the parse go to `warnings`.
pub fn parse(text: &str, base: &str, warnings: &mut Vec<String>) -> Result<ImportMap, String> {
    let json = parse_json(text).map_err(|why| format!("Invalid import map: {why}."))?;
    let Json::Obj(top) = &json else {
        return Err(String::from("Invalid import map: the top level must be a JSON object."));
    };
    let mut map = ImportMap::default();
    let mut budget = MAX_ENTRIES;
    if let Some(imports) = json.get("imports") {
        map.imports = normalize_table(imports, base, warnings, &mut budget)
            .map_err(|e| format!("Invalid import map: \"imports\": {e}."))?;
    }
    if let Some(scopes) = json.get("scopes") {
        let Json::Obj(list) = scopes else {
            return Err(String::from("Invalid import map: \"scopes\" must be a JSON object."));
        };
        for (prefix, table) in list {
            let Some(purl) = parse_url(prefix, Some(base)) else {
                warnings.push(format!("The scope prefix \"{prefix}\" in the import map is not a valid address."));
                continue;
            };
            let t = normalize_table(table, base, warnings, &mut budget)
                .map_err(|e| format!("Invalid import map: scope \"{prefix}\": {e}."))?;
            let key = purl.href();
            match map.scopes.iter_mut().find(|(k, _)| *k == key) {
                Some(e) => e.1 = t,
                None => map.scopes.push((key, t)),
            }
        }
        sort_desc(&mut map.scopes);
    }
    if let Some(i) = json.get("integrity")
        && !matches!(i, Json::Obj(_))
    {
        return Err(String::from("Invalid import map: \"integrity\" must be a JSON object."));
    }
    for (k, _) in top {
        if !matches!(k.as_str(), "imports" | "scopes" | "integrity") {
            warnings.push(format!("The import map has an unknown top-level key \"{k}\"; it was ignored."));
        }
    }
    Ok(map)
}

// ---- resolving ---------------------------------------------------------------------------------

/// "Resolve an imports match": the address for `normalized` in one table. `Ok(None)`: no
/// entry matches; `Err`: matched a blocked or unusable entry.
fn imports_match(
    normalized: &str,
    as_url: Option<&Url>,
    table: &Table,
) -> Result<Option<String>, ResolveError> {
    for (key, value) in table {
        if key == normalized {
            return match value {
                Some(v) => Ok(Some(v.clone())),
                None => Err(ResolveError(format!(
                    "Failed to resolve module specifier \"{normalized}\": the import map blocks it."
                ))),
            };
        }
        let usable = as_url.is_none_or(|u| is_special(&u.scheme));
        if key.ends_with('/') && normalized.starts_with(key.as_str()) && usable {
            let Some(addr) = value else {
                return Err(ResolveError(format!(
                    "Failed to resolve module specifier \"{normalized}\": the import map blocks \"{key}\"."
                )));
            };
            let after = &normalized[key.len()..];
            if !addr.ends_with('/') {
                return Err(ResolveError(format!(
                    "Failed to resolve module specifier \"{normalized}\": the address for \"{key}\" doesn't end with \"/\"."
                )));
            }
            let Some(url) = parse_url(after, Some(addr)) else {
                return Err(ResolveError(format!(
                    "Failed to resolve module specifier \"{normalized}\": \"{after}\" isn't a valid address under \"{addr}\"."
                )));
            };
            let href = url.href();
            if !href.starts_with(addr.as_str()) {
                return Err(ResolveError(format!(
                    "Failed to resolve module specifier \"{normalized}\": it leaves \"{addr}\"."
                )));
            }
            return Ok(Some(href));
        }
    }
    Ok(None)
}

/// "Resolve a module specifier": `specifier` as written in the module at address `base`.
pub fn resolve(
    map: &ImportMap,
    specifier: &str,
    base: &str,
) -> Result<String, ResolveError> {
    let bare_message = || {
        if map.is_empty() {
            format!(
                "Failed to resolve module specifier \"{specifier}\". Relative references must start with either \"/\", \"./\", or \"../\"."
            )
        } else {
            format!(
                "Failed to resolve module specifier \"{specifier}\": the import map has no entry for it, and it isn't a relative reference (\"/\", \"./\" or \"../\") or an address."
            )
        }
    };
    let as_url = if specifier.starts_with('/') || specifier.starts_with("./") || specifier.starts_with("../") {
        match parse_url(specifier, Some(base)) {
            Some(u) => Some(u),
            None => {
                return Err(ResolveError(format!(
                    "Failed to resolve module specifier \"{specifier}\": it isn't a valid address relative to {base}."
                )));
            }
        }
    } else {
        parse_url(specifier, None)
    };
    let normalized = match &as_url {
        Some(u) => u.href(),
        None => String::from(specifier),
    };
    for (prefix, table) in &map.scopes {
        if *prefix == base || (prefix.ends_with('/') && base.starts_with(prefix.as_str())) {
            if let Some(found) = imports_match(&normalized, as_url.as_ref(), table)? {
                return Ok(found);
            }
        }
    }
    if let Some(found) = imports_match(&normalized, as_url.as_ref(), &map.imports)? {
        return Ok(found);
    }
    match as_url {
        Some(u) => Ok(u.href()),
        None => Err(ResolveError(bare_message())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "https://example.com/app/index.html";

    fn map(text: &str) -> ImportMap {
        let mut w = Vec::new();
        parse(text, BASE, &mut w).unwrap_or_else(|e| panic!("{e}"))
    }

    fn res(m: &ImportMap, spec: &str, base: &str) -> Result<String, String> {
        resolve(m, spec, base).map_err(|e| e.0)
    }

    #[test]
    fn json_reader() {
        assert_eq!(parse_json(" {\"a\": [1, 2.5e1, -0, true, null], \"b\": \"x\\u0041\\n\"} ").unwrap().get("b"), Some(&Json::Str(String::from("xA\n"))));
        assert_eq!(parse_json("{\"a\":1,\"a\":2}").unwrap(), Json::Obj(alloc::vec![(String::from("a"), Json::Num(2.0))]));
        // A surrogate pair, a lone surrogate.
        assert_eq!(parse_json("\"\\ud83d\\ude00\"").unwrap(), Json::Str(String::from("\u{1F600}")));
        assert_eq!(parse_json("\"\\ud83d\"").unwrap(), Json::Str(String::from("\u{FFFD}")));
        for bad in ["", "{", "[1,]", "{\"a\"}", "{'a':1}", "01", "1.", "-", "\"a", "\"\\x\"", "tru", "{} x", "[1 2]", "\"a\nb\"", "{\"a\":1,}"] {
            assert!(parse_json(bad).is_err(), "{bad:?}");
        }
        // Depth and size limits.
        let deep = "[".repeat(100) + &"]".repeat(100);
        assert!(parse_json(&deep).is_err());
        let ok = "[".repeat(20) + &"]".repeat(20);
        assert!(parse_json(&ok).is_ok());
        let big = alloc::format!("\"{}\"", "a".repeat(MAX_MAP_BYTES));
        assert!(parse_json(&big).is_err());
    }

    #[test]
    fn plain_resolution_without_a_map() {
        let m = ImportMap::default();
        let b = "https://example.com/app/main.js";
        assert_eq!(res(&m, "./x.js", b).unwrap(), "https://example.com/app/x.js");
        assert_eq!(res(&m, "../y/z.js", b).unwrap(), "https://example.com/y/z.js");
        assert_eq!(res(&m, "/abs/q.js", b).unwrap(), "https://example.com/abs/q.js");
        assert_eq!(res(&m, "https://cdn.test/a.js", b).unwrap(), "https://cdn.test/a.js");
        assert_eq!(res(&m, "//cdn.test/a.js", b).unwrap(), "https://cdn.test/a.js");
        assert_eq!(res(&m, "./a.js?v=2#h", b).unwrap(), "https://example.com/app/a.js?v=2#h");
        let e = res(&m, "lodash", b).unwrap_err();
        assert!(e.contains("Failed to resolve module specifier \"lodash\"") && e.contains("Relative references"), "{e}");
        assert!(res(&m, "react/jsx-runtime", b).is_err());
        assert!(res(&m, "", b).is_err());
        // A scheme makes it an address (the fetch refuses the ones it can't do).
        assert_eq!(res(&m, "node:fs", b).unwrap(), "node:fs");
        assert!(res(&m, "./%zz/../a.js", b).is_ok());
    }

    #[test]
    fn imports_exact_and_prefix() {
        let m = map(r#"{"imports": {
            "lodash": "/vendor/lodash.js",
            "lib/": "/vendor/lib/",
            "lib/special/": "/special/",
            "blocked": null,
            "cdn": "https://cdn.test/x.js",
            "./local.js": "./other.js"
        }}"#);
        let b = "https://example.com/app/main.js";
        assert_eq!(res(&m, "lodash", b).unwrap(), "https://example.com/vendor/lodash.js");
        assert_eq!(res(&m, "lib/a/b.js", b).unwrap(), "https://example.com/vendor/lib/a/b.js");
        // The longer prefix wins.
        assert_eq!(res(&m, "lib/special/s.js", b).unwrap(), "https://example.com/special/s.js");
        assert_eq!(res(&m, "cdn", b).unwrap(), "https://cdn.test/x.js");
        assert!(res(&m, "blocked", b).unwrap_err().contains("blocks"));
        // URL-like keys are normalized: `./local.js` is the address it names (relative to the map).
        assert_eq!(res(&m, "./local.js", "https://example.com/app/m.js").unwrap(), "https://example.com/app/other.js");
        assert_eq!(res(&m, "https://example.com/app/local.js", b).unwrap(), "https://example.com/app/other.js");
        // No entry and bare: error mentions the map.
        let e = res(&m, "nothing", b).unwrap_err();
        assert!(e.contains("import map has no entry"), "{e}");
        // The prefix may not be escaped with `..`.
        let e = res(&m, "lib/../../etc/x.js", b).unwrap_err();
        assert!(e.contains("leaves"), "{e}");
    }

    #[test]
    fn prefix_mapping_applies_to_special_urls_too() {
        let m = map(r#"{"imports": {"https://cdn.test/lib/": "/local-lib/"}}"#);
        let b = "https://example.com/app/main.js";
        assert_eq!(res(&m, "https://cdn.test/lib/a.js", b).unwrap(), "https://example.com/local-lib/a.js");
        assert_eq!(res(&m, "https://cdn.test/other.js", b).unwrap(), "https://cdn.test/other.js");
        // A non-special scheme can't be prefix-matched.
        let m = map(r#"{"imports": {"node:": "/shims/"}}"#);
        assert_eq!(res(&m, "node:fs", b).unwrap(), "node:fs");
    }

    #[test]
    fn scopes() {
        let m = map(r#"{
            "imports": {"x": "/top/x.js", "y": "/top/y.js"},
            "scopes": {
              "/app/": {"x": "/app-scope/x.js"},
              "/app/deep/": {"x": "/deep-scope/x.js", "z": null},
              "https://other.test/": {"x": "/other/x.js"}
            }}"#);
        assert_eq!(res(&m, "x", "https://example.com/elsewhere/m.js").unwrap(), "https://example.com/top/x.js");
        assert_eq!(res(&m, "x", "https://example.com/app/m.js").unwrap(), "https://example.com/app-scope/x.js");
        // The most specific scope first.
        assert_eq!(res(&m, "x", "https://example.com/app/deep/m.js").unwrap(), "https://example.com/deep-scope/x.js");
        // A scope without the specifier falls through to the next, then the top level.
        assert_eq!(res(&m, "y", "https://example.com/app/deep/m.js").unwrap(), "https://example.com/top/y.js");
        assert_eq!(res(&m, "x", "https://other.test/m.js").unwrap(), "https://example.com/other/x.js");
        // A blocked entry in a matching scope is an error, not a fall-through.
        assert!(res(&m, "z", "https://example.com/app/deep/m.js").is_err());
        // The prefix is an address: only an exact match counts without a trailing slash.
        let m = map(r#"{"scopes": {"/app": {"x": "/exact/x.js"}}}"#);
        assert_eq!(res(&m, "x", "https://example.com/app").unwrap(), "https://example.com/exact/x.js");
        assert!(res(&m, "x", "https://example.com/app/m.js").is_err());
    }

    #[test]
    fn parse_problems() {
        let mut w = Vec::new();
        assert!(parse("[]", BASE, &mut w).is_err());
        assert!(parse("not json", BASE, &mut w).is_err());
        assert!(parse(r#"{"imports": []}"#, BASE, &mut w).is_err());
        assert!(parse(r#"{"scopes": 3}"#, BASE, &mut w).is_err());
        assert!(parse(r#"{"scopes": {"/a/": 3}}"#, BASE, &mut w).is_err());
        assert!(parse(r#"{"integrity": 3}"#, BASE, &mut w).is_err());
        w.clear();
        let m = parse(
            r#"{"imports": {"": "/a.js", "ok": "/ok.js", "num": 3, "badaddr": "http://", "dir/": "/nodir", "nul": null},
               "scopes": {"http://": {"a": "/a.js"}},
               "integrity": {"/ok.js": "sha384-abc"}, "extra": 1}"#,
            BASE,
            &mut w,
        )
        .unwrap();
        assert!(w.iter().any(|x| x.contains("empty string")), "{w:?}");
        assert!(w.iter().any(|x| x.contains("must be a string")), "{w:?}");
        assert!(w.iter().any(|x| x.contains("badaddr")), "{w:?}");
        assert!(w.iter().any(|x| x.contains("must end with")), "{w:?}");
        assert!(w.iter().any(|x| x.contains("scope prefix")), "{w:?}");
        assert!(w.iter().any(|x| x.contains("unknown top-level key \"extra\"")), "{w:?}");
        // The rest still works; the broken ones block.
        assert_eq!(res(&m, "ok", "https://example.com/m.js").unwrap(), "https://example.com/ok.js");
        assert!(res(&m, "num", "https://example.com/m.js").is_err());
        assert!(res(&m, "dir/x", "https://example.com/m.js").is_err());
        assert!(res(&m, "nul", "https://example.com/m.js").is_err());
    }

    #[test]
    fn keys_are_sorted_longest_first() {
        let m = map(r#"{"imports": {"a/": "/1/", "a/b/": "/2/", "a/b/c/": "/3/", "a": "/0.js"}}"#);
        let keys: Vec<&str> = m.imports.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["a/b/c/", "a/b/", "a/", "a"]);
        assert_eq!(res(&m, "a/b/c/d.js", "https://example.com/m.js").unwrap(), "https://example.com/3/d.js");
        assert_eq!(res(&m, "a/b/d.js", "https://example.com/m.js").unwrap(), "https://example.com/2/d.js");
        assert_eq!(res(&m, "a/x.js", "https://example.com/m.js").unwrap(), "https://example.com/1/x.js");
        assert_eq!(res(&m, "a", "https://example.com/m.js").unwrap(), "https://example.com/0.js");
    }

    #[test]
    fn the_map_is_bounded() {
        let mut text = String::from("{\"imports\": {");
        for i in 0..(MAX_ENTRIES + 500) {
            if i > 0 {
                text.push(',');
            }
            text.push_str(&alloc::format!("\"k{i}\": \"/v{i}.js\""));
        }
        text.push_str("}}");
        if text.len() <= MAX_MAP_BYTES {
            let mut w = Vec::new();
            let m = parse(&text, BASE, &mut w);
            assert!(m.is_err() || w.iter().any(|x| x.contains("too many")));
        }
    }
}
