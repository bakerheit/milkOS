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
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>{}</title><style>body {{ font-family: system-ui; margin: 16px 20px; }}</style></head><body>{body}</body></html>",
        esc(title)
    )
}

fn nav() -> &'static str {
    "<p><a href=\"milk:bookmarks\">Bookmarks</a> &middot; <a href=\"milk:history\">History</a> &middot; <a href=\"milk:downloads\">Downloads</a> &middot; <a href=\"milk:cookies\">Cookies</a></p>"
}

/// A new tab: what to do, and the bookmarks.
pub fn new_tab(b: &Bookmarks, online: bool) -> String {
    let mut s = String::from("<h1>New Tab</h1>");
    s.push_str(if online {
        "<p>Type an address in the bar above, or words to search for (Cmd-L).</p>"
    } else {
        "<p>Type an address in the bar above (Cmd-L). milkOS is offline, so only numeric addresses like http://10.0.2.2:8000/ and localhost open.</p>"
    });
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
pub fn text(title: &str, body: &str) -> String {
    page(title, &format!("<pre>{}</pre>", esc(body)))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(new_tab(&Bookmarks::default(), false).contains("offline"));
    }
}
