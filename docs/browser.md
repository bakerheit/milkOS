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
| files | `~/Library/Browser` and `~/Downloads` read-write; `/proc/net/tls` (a test boot's test root), `/proc/net/internet` and the theme setting read-only; `/dev/wm` and `/dev/clipboard`. Anything else only through the Powerbox |
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
  every tab, given to the engine as they come. PNG, BMP and PNM decode today (`user/imgdec`);
  other formats show their alt text.

Links the page doesn't colour itself are drawn in the link colour (#4F64A0, underlined; the
`link` token in appkit's Theme, SPEC "Type"). Pages are drawn on their own background in
both themes, so they use the light theme's token.

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

* `scripts/browserkit-test.sh` (`browserkit-unit`, smoke): cookies (domains, paths, Secure,
  SameSite, prefixes, expiry, third parties, limits, the file, hostile headers), history,
  bookmarks, the address bar, forms through the engine, downloads' names, certificate details,
  the built-in pages' escaping, painting (and pages of random tags that must paint without a
  panic) and zoom.
* `scripts/browser-test.sh` (default set, about two minutes): a VM against
  `scripts/browser_test_servers.py` on the Mac's 127.0.0.1 only (a DNS server for a made-up zone
  and the test pages). It checks the client protocol, a styled page and its stylesheet,
  scrolling, a fragment, find, zoom, back, a keyboard-driven GET form, a POST sign-in whose
  cookie comes back, third-party cookies refused, tabs, Cmd-click, `open URL`, bookmarks,
  downloads, error pages and the sandbox, from the serial log, the server's log and
  screenshots.
* `scripts/web-test.sh` and `scripts/tls-interop.sh` open pages by host name and over https
  (a good certificate and one for another site) through `open URL`.

## Known gaps

* Text is Unifont (8x16 cells, Latin-1, typographic characters folded): bold is faux, sizes
  are 1x / 2x / 3x the cell, so CSS font sizes snap. Proportional system fonts in user space
  are the next step (the engine already takes any measurer).
* No JavaScript, no `<iframe>`, no HTTP cache or HTTP/2, no file upload in forms, no
  `<video>` / `<audio>`; JPEG, GIF and WebP wait for user/imgdec.
* "Site" for cookies is the last two labels of the host (with a short list of two-label
  suffixes like `co.uk`), not the full public suffix list.
* No hover: client windows get pointer moves only while a button is down, so links don't
  show their address before a click (and there are no tooltips); the link's pressed colour
  token is unused for now.
* Closing the window can't be refused, so a download still running is lost when the window
  closes; Cmd-W closes the window, not the tab.
* A textarea's caret is always at the end of its text.
