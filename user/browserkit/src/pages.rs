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

/// The most of a page's source `view-source:` shows (bytes of text); the rest is said to be
/// there.
pub const SOURCE_CAP: usize = 4 << 20;
/// The most lines it shows.
pub const SOURCE_LINES: usize = 50_000;

/// The page of `view-source:`: `text` (the response body decoded), one numbered line each, in
/// the monospace font, as plain text (nothing in it is a tag, a link or a script). `address` is
/// what was fetched, `status` its status line, `bytes` the size of the response.
///
/// A line is wrapped to the width of the window by the page's own style (`pre-wrap`, and a
/// word cut anywhere when it has no space: a minified script is one line of a megabyte), with
/// its number hanging at the left of its first row, so every character can be read whatever
/// the width. The numbers are the style's counter, not text: copying a line takes only the line.
pub fn view_source(address: &str, status: &str, bytes: usize, text: &str) -> String {
    // What is shown: at most SOURCE_CAP bytes and SOURCE_LINES lines, cut at a character.
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
    let mut code = String::with_capacity(shown.len() + lines.len() * 24);
    for line in &lines {
        let line = line.strip_suffix('\r').unwrap_or(line);
        code.push_str("<div class=l>");
        for c in line.chars() {
            match c {
                '\t' => code.push('\t'),
                c if c.is_control() => code.push('\u{FFFD}'),
                '&' => code.push_str("&amp;"),
                '<' => code.push_str("&lt;"),
                '>' => code.push_str("&gt;"),
                c => code.push(c),
            }
        }
        code.push_str("</div>");
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
    // The gutter is as wide as the biggest number (digits of the last line, at 0.6em each in
    // the monospace font) and the code hangs from it.
    let digits = format!("{}", lines.len().max(1)).len();
    let gutter = digits as f32 * 0.62 + 0.6;
    let pad = gutter + 0.8;
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>view-source:{}</title><style>\
         html {{ color-scheme: light dark; }} body {{ margin: 0; font-family: system-ui; }} \
         .note {{ margin: 8px 16px; font-size: 13px; }} \
         .src {{ font-family: monospace; margin: 0; counter-reset: ln; }} \
         .l {{ counter-increment: ln; white-space: pre-wrap; overflow-wrap: anywhere; \
         padding: 0 16px 0 {pad}em; text-indent: -{pad}em; }} \
         .l::before {{ content: counter(ln); display: inline-block; width: {gutter}em; \
         margin-right: 0.8em; text-indent: 0; text-align: right; color: #8a7a6a; }}</style></head><body>\
         <p class=note>{note}</p>\
         <div class=src>{code}</div>{tail}</body></html>",
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
        // One block per line (the numbers are the style's counter): 4 lines, CRLF is one line
        // end, a tab stays, the markup is text.
        assert!(
            h.contains(
                "<div class=l>&lt;p a=\"1\"&gt;hi &amp; bye&lt;/p&gt;</div><div class=l>line 2</div><div class=l></div><div class=l>\tlast</div></div>"
            ),
            "{h}"
        );
        assert_eq!(h.matches("<div class=l>").count(), 4);
        assert!(!h.contains("<p a="), "nothing of the source is markup");
        assert!(h.contains("counter(ln)") && h.contains("white-space: pre-wrap"));
        assert!(h.contains("200 OK") && h.contains("40 bytes"));
        assert!(!h.contains("showing only"));
        // A text with a final newline has no empty line after it.
        let many: String = (0..12).map(|i| format!("l{i}\n")).collect();
        let h = view_source("a", "200 OK", 0, &many);
        assert_eq!(h.matches("<div class=l>").count(), 12);
        // Control characters are made visible, not passed on.
        let h = view_source("a", "200 OK", 0, "a\u{0}b\u{7}c");
        assert!(h.contains("a\u{FFFD}b\u{FFFD}c"));
        // Empty.
        assert!(view_source("a", "204 No Content", 0, "").contains("the response was empty"));
    }

    #[test]
    fn view_source_shows_a_very_long_line_whole_and_says_when_it_cut() {
        // A minified script: one line of 1 MB (more than the old limit of 512 KB) is all there,
        // in one block, for the style to wrap.
        let long = "x".repeat(1 << 20);
        let h = view_source("a", "200 OK", long.len(), &format!("short\n{long}\nend"));
        assert_eq!(h.matches("<div class=l>").count(), 3);
        assert!(h.contains(&format!("<div class=l>{long}</div>")));
        assert!(!h.contains("showing only"));
        // The gutter is wide enough for the last number.
        assert!(h.contains("width: 1.22em"), "{}", &h[..900]);
        // Too many bytes: the cap says so, at the top and the bottom.
        let big = "0123456789\n".repeat(SOURCE_CAP / 11 + 1000);
        let h = view_source("a", "200 OK", big.len(), &big);
        assert!(h.matches("showing only the first 4096 KB").count() >= 1, "{}", &h[..400]);
        assert!(h.contains("That is all that is shown"));
        assert!(h.len() < SOURCE_CAP * 3 + 5000, "bounded: {}", h.len());
        // One line past the cap is cut in the line, not dropped.
        let one = "y".repeat(SOURCE_CAP + 10_000);
        let h = view_source("a", "200 OK", one.len(), &one);
        assert!(h.contains(&"y".repeat(SOURCE_CAP)) && !h.contains(&"y".repeat(SOURCE_CAP + 1)));
        assert!(h.contains("showing only"));
        // Too many lines.
        let lines = "\n".repeat(SOURCE_LINES + 50);
        let h = view_source("a", "200 OK", lines.len(), &lines);
        assert!(h.contains("showing only the first 50000 lines"));
        assert_eq!(h.matches("<div class=l>").count(), SOURCE_LINES);
        // A multi-byte character at the cap is not cut in two.
        let tricky = "\u{e9}".repeat(SOURCE_CAP);
        let h = view_source("a", "200 OK", tricky.len(), &tricky);
        assert!(h.contains("showing only"));
    }

    #[test]
    fn view_source_of_hostile_text_is_always_one_block_a_line() {
        let mut x = 0xdead_beef_1234_5678u64;
        let mut rnd = |n: u64| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x % n
        };
        let pool = ["a", "<", ">", "&", "\"", "'", "\n", "\r\n", "\t", "\u{0}", "\u{7f}", "é", "日本", "\u{1F600}", " ", "</pre>", "<script>", "\u{FEFF}"];
        for _ in 0..300 {
            let mut t = String::new();
            for _ in 0..rnd(400) {
                t.push_str(pool[rnd(pool.len() as u64) as usize]);
                if rnd(40) == 0 {
                    t.push_str(&"x".repeat(rnd(500) as usize));
                }
            }
            let h = view_source("http://a.test/<x>", "200 OK", t.len(), &t);
            // Nothing of the text is markup: no tag from the pool survives, and the blocks are
            // the lines (a line break in the text never makes another block).
            assert!(!h.contains("<script>") && !h.contains("</pre>"), "{h}");
            let body = h.split("<div class=src>").nth(1).unwrap().split("</div></body>").next().unwrap();
            let lines = t.strip_suffix('\n').unwrap_or(&t).split('\n').count();
            assert_eq!(body.matches("<div class=l>").count(), lines.min(SOURCE_LINES), "{h}");
            assert_eq!(body.matches("</div>").count(), lines.min(SOURCE_LINES) + 0, "{h}");
        }
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
