# Browser

`/Apps/browser/browser [--log-input] [ADDRESS]`, the dock's globe, `open URL` and Skim's
Browser entry. The window is `user/browser`; its rules (cookies, history, bookmarks, the
address bar, forms, downloads' names, certificate details, the built-in pages, painting and
zoom) are `user/browserkit`, host-tested (`scripts/browserkit-test.sh`); pages are parsed,
styled and laid out by `user/webengine` (HTML5, CSS, layout; its interface is
`user/webengine/src/api.rs`). Fetching is milkweb's HTTP client and the TLS 1.3 client
(docs/networking.md, docs/tls.md) through `rt::web` / `rt::tls`. There is no JavaScript.

## The sandbox

Pages are hostile input, so the Browser sandboxes itself before it fetches anything:

| | |
| --- | --- |
| files | `~/Library/Browser` and `~/Downloads` read-write; `/System/Fonts` (the system fonts, read ahead with `gui::text::preload`), `/proc/net/tls` (a test boot's test root), `/proc/net/internet` and the theme setting read-only; `/dev/wm` and `/dev/clipboard`. Anything else only through the Powerbox |
| network | all (it is a browser) |
| spawning | none |

`/proc/<pid>/status`'s `Caps:` line shows it; `scripts/browser-test.sh` checks it.

## What it does

* **Tabs**: Cmd-T new tab (the address bar gets the keyboard), Ctrl-W or the tab's x closes
  one (Cmd-W is the window system's and closes the window), Ctrl-Tab / Ctrl-Shift-Tab and
  Cmd-1..9 switch, a middle click on a tab closes it, Cmd-click (or the middle button, or a
  link with `target=_blank`) opens a link in a new tab.
* **The address bar** (Cmd-L): an address (no scheme means http), `milk:newtab`,
  `milk:bookmarks`, `milk:history`, `milk:downloads`, `milk:cookies`, or bare words, which
  become a DuckDuckGo search (offline, a page says searching needs the internet). Cmd-C /
  Cmd-V copy and paste in it.
* **History**: back / forward per tab (the buttons, Cmd-[ / Cmd-], Cmd-Left / Cmd-Right,
  Backspace), each page coming back where it was scrolled; reload (Cmd-R) and stop (Esc,
  Cmd-.). Visited pages are listed on `milk:history` (kept in `~/Library/Browser/history`).
* **Bookmarks**: Cmd-D or the ribbon at the end of the address bar toggles one; Cmd-B lists
  them; the new tab page shows the latest. `~/Library/Browser/bookmarks`, one
  `address <TAB> title` a line.
* **Scrolling**: Page Up / Page Down, Space / Shift-Space, Home / End, the arrows, and the
  wheel, all smooth; a `#fragment` scrolls to its element (also within the page). Skip to
  Content (the menu) goes to `<main>` / `role=main`.
* **Find** (Cmd-F): every match highlighted, Enter / Cmd-G next, Shift for the previous one,
  Esc closes the bar.
* **Zoom**: Cmd-+ / Cmd-- through 50% to 300%, Cmd-0 back to 100%. The page is laid out for
  the narrower (or wider) width and scaled: whole-number zooms stay crisp, the others are
  smoothed.
* **Forms**: text, password, checkbox, radio (the arrows move along a group), select (a
  list, or Up / Down), textarea, submit / reset buttons; Tab and Shift-Tab move between
  them, Enter sends the form, Esc leaves a field. GET and POST
  (`application/x-www-form-urlencoded`), the encoding and the successful controls are the
  engine's. A password sent over plain http gets a note in the status line.
* **Cookies** (`browserkit::cookies`, RFC 6265 basics): `Domain`, `Path`, `Secure` (only from
  and to https), `HttpOnly` (kept; there are no scripts to hide them from), `Expires` /
  `Max-Age`, `SameSite` (Lax when a site says nothing: a page reached from another site's
  link or form gets Lax cookies for a GET only, Strict ones never; `None` needs `Secure`), the
  `__Secure-` and `__Host-` prefixes, a plain-http page can't replace a secure cookie.
  **No third-party cookies**: an image or stylesheet from another site gets no cookies and
  can't set any. Limits: 4 KB a cookie, 50 a site, 3000 in all. Persistent cookies are kept
  in `~/Library/Browser/cookies` (tab separated); session cookies end with the Browser.
  `milk:cookies` lists them by site; Cmd-Shift-Delete forgets cookies and history.
* **Downloads**: an answer that isn't a page (a type other than HTML or text, or
  `Content-Disposition: attachment`) is saved to `~/Downloads` under its name (from
  `filename*` / `filename`, else the address, made safe; a taken name gets ` (2)`, ...),
  with a progress line in the status bar and `milk:downloads` listing them. Alt-click (or a
  link with `download`) saves whatever a link points at. Up to 64 MB.
* **Security**: https pages show a padlock; clicking it shows who the certificate was issued
  to and by, its dates, its names and what TLS negotiated. Plain http pages say `http` there
  and the panel says the page isn't secure. Images and stylesheets over plain http on an
  https page are not loaded. A refused certificate, a refused connection, an unknown host,
  a timeout and the rest get a page saying so in words, with a hint.
* **Images and stylesheets**: fetched on their own (a few at a time, pages first), cached for
  every tab, given to the engine as they come, decoded by `user/imgdec` on the fetching thread:
  PNG, JPEG (baseline and progressive), GIF, WebP (lossy and lossless), BMP, PNM. Animated GIF
  and WebP play (`imgdec::anim::Animation`, a frame at a time with its delay) while their tab is
  in front; a background tab's pictures stand still. A picture that doesn't decode shows its
  alt text.
* **Text**: in the system fonts (`gui::text`, the desktop's five families, proportional and
  anti-aliased, with Latin Extended, Greek, Cyrillic, punctuation and symbols): CSS
  `sans-serif` and `system-ui` are the family Settings > Appearance chose (so pages follow a
  Settings change, like the chrome), `serif` IBM Plex Sans (no serif face is baked), `cursive`
  Atkinson Hyperlegible Next, `fantasy` Ubuntu Sans. Sizes snap to the fonts' 13, 26 and 52 px;
  weight 600 and up is bold, 500 medium; italics are drawn oblique. Monospace (`<pre>`,
  `<code>`, `<tt>`, CSS `monospace`) is Unifont 8x16, with characters Unifont lacks taken from
  the system font. `browserkit::font` is the measurer the engine lays out with, so lines break
  where the glyphs are drawn.

**Colours.** The engine's defaults are milkOS's (SPEC "Type"): a milk #FFFDF8 canvas,
espresso #3A2E28 text, links #4F64A0 (underlined), never pure white or black; a page's own
colours win. On a dark desktop a page gets the dark defaults (canvas #2E2520, text #F4EBE0,
links #A9B8E8) only when it opts into a dark scheme (CSS `color-scheme: dark` / `light dark`
on the root, or `<meta name=color-scheme>`) or sets no styles and no colour attributes at all;
a page that sets some colours but no scheme stays light, since dark text on a dark canvas is
worse than a light page in dark chrome. `prefers-color-scheme: dark` matches on a dark
desktop either way. The built-in pages opt in. appkit's Theme has the link and pressed-link
tokens for both themes.

There are no app menus for client windows yet, so the toolbar's menu button (three lines)
lists everything, with its key.

## Inside

* `net.rs`: every page, image, stylesheet and download is a job on its own thread (512 KiB
  stacks for the TLS handshake, at most six at once). A job runs `milkweb::http::fetch` with
  the Browser's hooks: the cookie jar asked and fed on every hop, progress, Stop, and a page
  that turns out to be a file becomes a download, written by the job itself.
* `web.rs`: the engine's `Document` behind the calls the app makes; `draw.rs` paints the
  chrome and, through `browserkit::paint`, the display list and the form controls (inside the
  boxes the engine painted); `tab.rs` and `store.rs` (the profile files, written to a
  temporary name and renamed).
* `open URL` while the Browser runs: the desktop appends the address to
  `~/Library/Browser/open` and brings the window forward; the Browser reads and empties that
  file (it checks twice a second) and opens a tab for each address.
* `--log-input` prints every key, click and wheel event with its modifier keys, which
  `scripts/browser-test.sh` uses to check the client protocol (Home / End / Page Up / Page
  Down reach client windows; clicks, drags and the wheel carry the modifiers).

## Tests

* `scripts/browserkit-test.sh` (`browserkit-unit`, smoke): text in the real system fonts
  (fontkit over the baked blobs: Greek, Cyrillic and Latin Extended draw without boxes, find
  highlights are as wide as the text they cover), cookies (domains, paths, Secure,
  SameSite, prefixes, expiry, third parties, limits, the file, hostile headers), history,
  bookmarks, the address bar, forms through the engine, downloads' names, certificate details,
  the built-in pages' escaping, painting (and pages of random tags that must paint without a
  panic) and zoom.
* `scripts/browser-test.sh` (default set, about two minutes): a VM against
  `scripts/browser_test_servers.py` on the Mac's 127.0.0.1 only (a DNS server for a made-up zone
  and the test pages). It checks the client protocol, that `/dev/wm` lists the window at once,
  a styled page and its stylesheet, a Greek and Cyrillic page, JPEG, GIF and WebP pictures (the
  animated ones play, and stand still in a background tab),
  scrolling, a fragment, find, zoom, back, a keyboard-driven GET form, a POST sign-in whose
  cookie comes back, third-party cookies refused, tabs, Cmd-click, `open URL`, bookmarks,
  downloads, error pages and the sandbox, from the serial log, the server's log and
  screenshots.
* `scripts/web-test.sh` and `scripts/tls-interop.sh` open pages by host name and over https
  (a good certificate and one for another site) through `open URL`.

## Known gaps

* Font sizes snap to 13, 26 and 52 px (the baked faces), so `font-size: 16px` text is 13 px;
  there are no real italics or serif faces; CJK and other scripts no family has show as boxes.
* No JavaScript, no `<iframe>`, no HTTP cache or HTTP/2, no file upload in forms, no
  `<video>` / `<audio>`; arithmetic-coded or 12-bit JPEG, colour profiles.
* "Site" for cookies is the last two labels of the host (with a short list of two-label
  suffixes like `co.uk`), not the full public suffix list.
* No hover: client windows get pointer moves only while a button is down, so links don't
  show their address before a click (and there are no tooltips); links have no pressed
  colour yet.
* Closing the window can't be refused, so a download still running is lost when the window
  closes; Cmd-W closes the window, not the tab.
* A textarea's caret is always at the end of its text.
