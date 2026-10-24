//! The Browser's own pages, as HTML for the page renderer: a new tab, bookmarks, history,
//! downloads, cookies, and the error pages ("Server not found", a bad certificate, ...).

use alloc::format;
use alloc::string::String;

use crate::bookmarks::Bookmarks;
use crate::cookies::{Jar, site_of};
use crate::history::Visits;

/// `s` with `& < > "` escaped, for text and attribute values.
pub fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            c => o.push(c),
        }
    }
    o
}

fn page(title: &str, body: &str) -> String {
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>{}</title><style>html {{ color-scheme: light dark; }} body {{ font-family: system-ui; margin: 16px 20px; }}</style></head><body>{body}</body></html>",
        esc(title)
    )
}

fn nav() -> &'static str {
    "<p><a href=\"milk:bookmarks\">Bookmarks</a> &middot; <a href=\"milk:history\">History</a> &middot; <a href=\"milk:downloads\">Downloads</a> &middot; <a href=\"milk:cookies\">Cookies</a></p>"
}

/// A new tab: what to do (words search with `engine`, by name), and the bookmarks.
pub fn new_tab(b: &Bookmarks, online: bool, engine: &str) -> String {
    let mut s = String::from("<h1>New Tab</h1>");
    if online {
        s.push_str(&format!(
            "<p>Type an address in the bar above, or words to search for with {} (Cmd-L). To search somewhere else, pick another search engine in Settings &gt; Browser.</p>",
            esc(engine)
        ));
    } else {
        s.push_str("<p>Type an address in the bar above (Cmd-L). milkOS is offline, so only numeric addresses like http://10.0.2.2:8000/ and localhost open.</p>");
    }
    if !b.0.is_empty() {
        s.push_str("<h2>Bookmarks</h2><ul>");
        for m in b.0.iter().rev().take(20) {
            s.push_str(&format!(
                "<li><a href=\"{}\">{}</a></li>",
                esc(&m.url),
                esc(&m.title)
            ));
        }
        s.push_str("</ul>");
    }
    s.push_str(nav());
    page("New Tab", &s)
}

pub fn bookmarks(b: &Bookmarks) -> String {
    let mut s = String::from("<h1>Bookmarks</h1>");
    if b.0.is_empty() {
        s.push_str("<p>No bookmarks yet. Cmd-D (or the star in the address bar) bookmarks the page you're on.</p>");
    } else {
        s.push_str("<ul>");
        for m in &b.0 {
            s.push_str(&format!(
                "<li><a href=\"{}\">{}</a><br><small>{}</small></li>",
                esc(&m.url),
                esc(&m.title),
                esc(&m.url)
            ));
        }
        s.push_str("</ul><p>To remove one, open it and press Cmd-D again.</p>");
    }
    s.push_str(nav());
    page("Bookmarks", &s)
}

pub fn history(v: &Visits) -> String {
    let mut s = String::from("<h1>History</h1>");
    if v.0.is_empty() {
        s.push_str("<p>Nothing visited yet.</p>");
    } else {
        s.push_str("<ul>");
        let mut day = String::new();
        for x in v.0.iter().rev().take(200) {
            let d = crate::date::day_month_year(x.when);
            if d != day {
                s.push_str(&format!("</ul><h3>{}</h3><ul>", esc(&d)));
                day = d;
            }
            let t = if x.title.is_empty() { &x.url } else { &x.title };
            s.push_str(&format!(
                "<li><a href=\"{}\">{}</a></li>",
                esc(&x.url),
                esc(t)
            ));
        }
        s.push_str("</ul>");
    }
    s.push_str(nav());
    page("History", &s)
}

/// One row of the Downloads page.
pub struct DownloadRow {
    pub name: String,
    pub url: String,
    /// "Done, 1.2 MB", "34% of 3.4 MB", "Failed: ...".
    pub state: String,
}

pub fn downloads(rows: &[DownloadRow], folder: &str) -> String {
    let mut s = String::from("<h1>Downloads</h1>");
    s.push_str(&format!("<p>Files are saved in {}.</p>", esc(folder)));
    if rows.is_empty() {
        s.push_str("<p>Nothing downloaded yet.</p>");
    } else {
        s.push_str("<table><tr><th>File</th><th>State</th></tr>");
        for r in rows.iter().rev() {
            s.push_str(&format!(
                "<tr><td><a href=\"{}\">{}</a></td><td>{}</td></tr>",
                esc(&r.url),
                esc(&r.name),
                esc(&r.state)
            ));
        }
        s.push_str("</table>");
    }
    s.push_str(nav());
    page("Downloads", &s)
}

pub fn cookies(j: &Jar) -> String {
    let mut s = String::from("<h1>Cookies</h1>");
    s.push_str("<p>Sites can only keep cookies for themselves: the Browser refuses third-party cookies, and a cookie without SameSite counts as Lax.</p>");
    if j.is_empty() {
        s.push_str("<p>No cookies.</p>");
    } else {
        let mut sites: alloc::vec::Vec<String> =
            j.all().iter().map(|c| site_of(&c.domain)).collect();
        sites.sort();
        sites.dedup();
        s.push_str("<table><tr><th>Site</th><th>Cookies</th></tr>");
        for site in &sites {
            let names: alloc::vec::Vec<String> = j
                .all()
                .iter()
                .filter(|c| site_of(&c.domain) == *site)
                .map(|c| {
                    let mut n = c.name.clone();
                    if c.secure {
                        n.push_str(" (secure)");
                    }
                    if c.expires.is_none() {
                        n.push_str(" (session)");
                    }
                    n
                })
                .collect();
            s.push_str(&format!(
                "<tr><td>{}</td><td>{}</td></tr>",
                esc(site),
                esc(&names.join(", "))
            ));
        }
        s.push_str("</table><p>Cmd-Shift-Delete forgets them all.</p>");
    }
    s.push_str(nav());
    page("Cookies", &s)
}

/// An error page: a heading, what happened, what to try.
pub fn error(title: &str, message: &str, hint: &str, address: &str) -> String {
    let mut s = format!("<h1>{}</h1>", esc(title));
    if !message.is_empty() {
        s.push_str(&format!("<p>{}</p>", esc(message)));
    }
    if !hint.is_empty() {
        s.push_str(&format!("<p>{}</p>", esc(hint)));
    }
    if !address.is_empty() {
        s.push_str(&format!("<p><small>{}</small></p>", esc(address)));
    }
    s.push_str("<p>Reload (Cmd-R) to try again.</p>");
    page(title, &s)
}

/// Plain text shown as a page.
/// The most of a page's source `view-source:` shows (bytes of text); the rest is said to be
/// there.
pub const SOURCE_CAP: usize = 512 << 10;
/// The most lines it shows.
pub const SOURCE_LINES: usize = 20_000;
/// A line longer than this many characters is shown as several (the continuation lines
/// have no number).
pub const SOURCE_WRAP: usize = 160;

/// The page of `view-source:`: `text` (the response body decoded), one numbered line each, in
/// the monospace font, as plain text (nothing in it is a tag, a link or a script). `address` is
/// what was fetched, `status` its status line, `bytes` the size of the response.
pub fn view_source(address: &str, status: &str, bytes: usize, text: &str) -> String {
    // What is shown: at most SOURCE_CAP bytes and SOURCE_LINES lines, cut at a line end when
    // there is one near, else at a character.
    let mut end = text.len().min(SOURCE_CAP);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let cut_bytes = end < text.len();
    let shown = &text[..end];
    let mut lines: alloc::vec::Vec<&str> = shown.split('\n').collect();
    // (A text that ends with a newline has an empty last piece: not a line of its own.)
    if lines.last() == Some(&"") && lines.len() > 1 {
        lines.pop();
    }
    let cut_lines = lines.len() > SOURCE_LINES;
    lines.truncate(SOURCE_LINES);
    let width = format!("{}", lines.len().max(1)).len();
    let mut gutter = String::with_capacity(lines.len() * (width + 1));
    let mut code = String::with_capacity(shown.len() + lines.len() * 2);
    for (i, line) in lines.iter().enumerate() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let mut first = true;
        let mut chars = line.chars().peekable();
        loop {
            // The next piece of at most SOURCE_WRAP characters.
            let mut n = 0;
            let mut piece = String::new();
            while n < SOURCE_WRAP && let Some(&c) = chars.peek() {
                chars.next();
                n += 1;
                match c {
                    '\t' => piece.push('\t'),
                    c if c.is_control() => piece.push('\u{FFFD}'),
                    '&' => piece.push_str("&amp;"),
                    '<' => piece.push_str("&lt;"),
                    '>' => piece.push_str("&gt;"),
                    c => piece.push(c),
                }
            }
            if first {
                gutter.push_str(&format!("{:>width$}\n", i + 1));
                first = false;
            } else {
                gutter.push_str(&format!("{:>width$}\n", ""));
            }
            code.push_str(&piece);
            code.push('\n');
            if chars.peek().is_none() {
                break;
            }
        }
    }
    let mut note = format!("{} &middot; {} &middot; {} bytes", esc(address), esc(status), bytes);
    let mut tail = String::new();
    if cut_bytes || cut_lines {
        let what = if cut_bytes {
            format!("the first {} KB of {} KB", SOURCE_CAP >> 10, text.len().div_ceil(1024))
        } else {
            format!("the first {SOURCE_LINES} lines")
        };
        note.push_str(&format!(" &middot; <b>showing only {what}</b>"));
        tail = format!("<p class=note><b>That is all that is shown: only {what}.</b> The rest of the source is not displayed.</p>");
    }
    if text.is_empty() {
        note.push_str(" &middot; the response was empty");
    }
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>view-source:{}</title><style>\
         html {{ color-scheme: light dark; }} body {{ margin: 0; font-family: system-ui; }} \
         .note {{ margin: 8px 16px; font-size: 13px; }} \
         table {{ border-collapse: collapse; margin: 0; }} td {{ vertical-align: top; padding: 0; }} \
         pre {{ margin: 0; font-family: monospace; }} \
         .n {{ color: #8a7a6a; padding: 0 10px 0 16px; border-right: 1px solid #c8b8a0; text-align: right; }} \
         .c {{ padding: 0 16px 0 10px; }}</style></head><body>\
         <p class=note>{note}</p>\
         <table><tr><td class=n><pre>{gutter}</pre></td><td class=c><pre>{code}</pre></td></tr></table>{tail}</body></html>",
        esc(address)
    )
}

pub fn text(title: &str, body: &str) -> String {
    page(title, &format!("<pre>{}</pre>", esc(body)))
}

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn view_source_numbers_escapes_and_caps() {
        let h = view_source("http://a.test/x", "200 OK", 40, "<p a=\"1\">hi & bye</p>\r\nline 2\n\n\tlast");
        assert!(h.contains("<title>view-source:http://a.test/x</title>"));
        // The gutter: 4 lines, numbers right-aligned in one column.
        assert!(h.contains("<pre>1\n2\n3\n4\n</pre>"), "{h}");
        // The text is escaped, CRLF is one line end, a tab stays.
        assert!(h.contains("<pre>&lt;p a=\"1\"&gt;hi &amp; bye&lt;/p&gt;\nline 2\n\n\tlast\n</pre>"), "{h}");
        assert!(!h.contains("<p a="), "nothing of the source is markup");
        assert!(h.contains("200 OK") && h.contains("40 bytes"));
        assert!(!h.contains("showing only"));
        // A text with a final newline has no empty line after it; many lines widen the gutter.
        let many: String = (0..12).map(|i| format!("l{i}\n")).collect();
        let h = view_source("a", "200 OK", 0, &many);
        assert!(h.contains("<pre> 1\n 2\n"), "{h}");
        assert!(h.contains("11\n12\n</pre>"), "{h}");
        assert!(!h.contains("13\n"));
        // Control characters are made visible, not passed on.
        let h = view_source("a", "200 OK", 0, "a\u{0}b\u{7}c");
        assert!(h.contains("a\u{FFFD}b\u{FFFD}c"));
        // Empty.
        assert!(view_source("a", "204 No Content", 0, "").contains("the response was empty"));
    }

    #[test]
    fn view_source_wraps_long_lines_and_says_when_it_cut() {
        let long = "x".repeat(SOURCE_WRAP * 2 + 5);
        let h = view_source("a", "200 OK", long.len(), &format!("short\n{long}\nend"));
        // 5 display lines for 3 numbered ones; continuation lines have no number.
        assert!(h.contains("<pre>1\n2\n \n \n3\n</pre>"), "{h}");
        assert_eq!(h.matches(&"x".repeat(SOURCE_WRAP)).count(), 2);
        // Too many bytes: the cap says so, at the top and the bottom.
        let big = "0123456789\n".repeat(SOURCE_CAP / 11 + 1000);
        let h = view_source("a", "200 OK", big.len(), &big);
        assert!(h.matches("showing only the first 512 KB").count() >= 1, "{}", &h[..400]);
        assert!(h.contains("That is all that is shown"));
        assert!(h.len() < SOURCE_CAP * 3 + 5000, "bounded: {}", h.len());
        // Too many lines.
        let lines = "\n".repeat(SOURCE_LINES + 50);
        let h = view_source("a", "200 OK", lines.len(), &lines);
        assert!(h.contains("showing only the first 20000 lines"));
        // A multi-byte character at the cap is not cut in two.
        let tricky = "é".repeat(SOURCE_CAP);
        let h = view_source("a", "200 OK", tricky.len(), &tricky);
        assert!(h.contains("showing only"));
    }

    #[test]
    fn escaping_and_pages() {
        assert_eq!(
            esc("<a href=\"x\">&</a>"),
            "&lt;a href=&quot;x&quot;&gt;&amp;&lt;/a&gt;"
        );
        let mut b = Bookmarks::default();
        b.toggle("http://a.test/?q=<x>", "A <b>bold</b> title");
        let h = bookmarks(&b);
        assert!(h.contains("href=\"http://a.test/?q=&lt;x&gt;\""));
        assert!(h.contains("A &lt;b&gt;bold&lt;/b&gt; title"));
        let e = error(
            "Server not found",
            "nope.test: there's no such host",
            "Check the spelling.",
            "http://nope.test/",
        );
        assert!(e.contains("<title>Server not found</title>"));
        assert!(new_tab(&Bookmarks::default(), false, "Bing").contains("offline"));
        let on = new_tab(&Bookmarks::default(), true, "Brave Search");
        assert!(on.contains("search for with Brave Search"), "{on}");
        assert!(on.contains("Settings &gt; Browser"));
        assert!(new_tab(&Bookmarks::default(), true, "<b>").contains("with &lt;b&gt;"));
    }
}
