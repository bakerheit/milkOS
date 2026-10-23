# Browser

`/Apps/browser/browser [--log-input] [ADDRESS]`, the dock's globe, `open URL` and Skim's
Browser entry. The window is `user/browser`; its rules (cookies, history, bookmarks, the
address bar, forms, downloads' names, certificate details, the built-in pages, painting and
zoom) are `user/browserkit`, host-tested (`scripts/browserkit-test.sh`); pages are parsed,
styled and laid out by `user/webengine` (HTML5, CSS, layout; its interface is
`user/webengine/src/api.rs`). Fetching is milkweb's HTTP client and the TLS 1.3 client
(docs/networking.md, docs/tls.md) through `rt::web` / `rt::tls`. Pages' JavaScript runs in
`user/jsengine` (its interface is `user/jsengine/src/api.rs`), bound to the page by
`user/webengine/src/script` (see JavaScript, below).

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
  and to https), `HttpOnly` (never shown to scripts, which can't set or replace one either), `Expires` /
  `Max-Age`, `SameSite` (Lax when a site says nothing: a page reached from another site's
  link or form gets Lax cookies for a GET only, Strict ones never; `None` needs `Secure`), the
  `__Secure-` and `__Host-` prefixes, a plain-http page can't replace a secure cookie.
  **No third-party cookies**: an image or stylesheet from another site gets no cookies and
  can't set any. Limits: 4 KB a cookie, 50 a site, 3000 in all. Persistent cookies are kept
  in `~/Library/Browser/cookies` (tab separated); session cookies end with the Browser.
  `milk:cookies` lists them by site; Cmd-Shift-Delete forgets cookies and history.
* **Script networking rules** (`browserkit::netpolicy`, pure and host-tested): what a page's
  `fetch` / XMLHttpRequest may do, the web's same-origin policy and CORS as the Fetch standard
  writes them. `Exchange` takes one request hop by hop (what to send, then what to make of the
  answer); the pieces are public too. Origins are scheme, host and port; only http, https,
  `data:` (GET, decoded in place) and the page's own `blob:` addresses exist, and addresses with
  a user name, a backslash, a number-like host (`127.1`) or one of the refused ports (mail, ssh,
  ...) are errors. Modes `same-origin`, `cors` and `no-cors` (an opaque answer: status 0, no
  headers, no body), credentials `omit` / `same-origin` / `include`, redirects `follow` /
  `error` / `manual` (at most 20, `Authorization` dropped across origins, `Origin: null` once a
  redirect goes from one foreign origin to another). Requests that are not simple (other
  methods, custom headers, JSON bodies) are preflighted, and the answer is cached; a response's
  headers reach the page only if CORS-safelisted or exposed, `Set-Cookie` never. Cookies go
  only to the page's own site (no third-party cookies, also not after a redirect through
  another site) and are kept only after the CORS check passed. An https page may not request
  http; a page on a public host may not request a private-network or loopback address, and
  `Hop::check_resolved` lets the transport refuse a name that resolves there. Limits: 4 MiB
  request body, 8 MiB response, 64 headers, 20 redirects, 30 s, 16 requests per tab and 6 per
  host (more wait in line). Failures print a plain-English line for the console; the page gets
  only "Failed to fetch". The module docs list every place it is stricter than the standard.
  Known gaps: no full public suffix list (the cookie code's last-two-labels rule), no IDNA, no
  HTTP cache or CSP.
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
  Atkinson Hyperlegible Next, `fantasy` Ubuntu Sans. A page that names no font (or only fonts
  it doesn't have) gets sans-serif: the Settings font, not serif. Sizes snap to the fonts' 13, 26 and 52 px;
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

## JavaScript

Web pages run their scripts (the toolbar's menu turns JavaScript off and on; the choice is
kept in `~/Library/Browser/settings`, and the page reloads). Stage 1 is what ordinary
interactive pages need: menus, tabs, toggles, form validation, content built by script.

* **Scripts**: inline and external `<script>`s, in the HTML order: blocking ones in tree
  order, then `defer` ones, then `DOMContentLoaded`, and `load` once the stylesheets and
  images are in; `async` ones as soon as they are fetched; scripts a page inserts run on
  insertion (external ones fetched like stylesheets, async unless `async = false`).
  `document.readyState` and `document.currentScript` follow. The page is parsed whole
  before any script runs, so `document.write` during loading puts its markup after the
  running script. Data blocks
  (`application/json`, `text/template`) never run; `<noscript>` is raw text and hidden while
  scripts are on, and shown when they are off.
* **Modules** (`user/webengine/src/script/modrt.rs`, `modules.rs`, `importmap.rs`): real ES
  modules. `<script type=module>`, inline or with `src`, is deferred (in document order with
  the `defer` scripts), `async` runs it as soon as its imports are in, and `nomodule` classic
  scripts are skipped. A module and every module it imports are fetched with mode `cors` and
  `same-origin` credentials (`crossorigin=use-credentials` for the ones that pass them on), so
  the same rules as for `fetch` apply: another origin needs `Access-Control-Allow-Origin`,
  redirects are judged hop by hop, mixed content is refused. The response has to be a success
  with a JavaScript MIME type (a JSON one for `import data from "./x.json" with { type: "json" }`);
  anything else is a console error and an `error` event on the `<script>`, and the other modules
  carry on. Specifiers are resolved against the importing module's (final) address; a bare one
  (`import "lodash"`) fails with the web's TypeError unless the page's **import map** maps it
  (`imports`, `scopes`, trailing-slash prefixes, `null` to block; `integrity` is read and
  ignored). Only the first `<script type=importmap>` counts, only an inline one, and none can
  be added after the first module load has begun (the console says so). `import()` works from
  classic scripts and modules through the same map and loader, `import.meta.url` and
  `import.meta.resolve()` are there, and `document.currentScript` is null in a module. One
  module map per page keyed by address and type: each module is fetched, compiled and run once,
  cycles and top-level `await` follow the spec (a pending `await` doesn't hold up sibling
  scripts, `DOMContentLoaded` or `load`). A page may have 2,000 modules, an import chain 256
  deep, 24 module fetches in flight and 48 MB of module source in all, and the module graph
  counts toward the page's heap budget; going to another page cancels fetches still running.
  A syntax error or an unresolvable import reaches `window.onerror` and stops that graph only.
  Not done: CSS module scripts, `import.meta` beyond `url` and `resolve`, multiple import maps
  (merged, as the newest standard has them), `integrity` checks, workers and `data:`/`blob:`
  module addresses.
* **The DOM** (webengine's DOM, live): document lookups and `querySelector(All)` (the
  engine's own selector matcher), creating, inserting, moving, cloning and removing nodes,
  `textContent`, `innerHTML` / `outerHTML` / `insertAdjacentHTML` through the HTML fragment
  parsing algorithm (scripts in it don't run), `innerText`, attributes, `id` / `className`
  / `classList` / `dataset` / `style` (the inline style attribute), live collections
  (`children`, `childNodes`, `getElementsBy*`, `forms`, `form.elements`, `select.options`),
  the form controls' `value` / `checked` / `disabled` / `selectedIndex` (the same state the
  Browser draws and submits), constraint validation (`required`, types, lengths, ranges,
  `pattern`, `setCustomValidity`; a form that fails isn't sent, its first bad field gets the
  focus and the status line the reason), `getBoundingClientRect`, `offset*` / `client*` /
  `scroll*` sizes, `scrollIntoView`, `focus` / `blur`, `click()`, `getComputedStyle` (some
  properties) and `matchMedia`.
* **Events**: `addEventListener` with capture, bubbling, `once`, `passive`,
  `preventDefault` and `stopPropagation`; `on*` attributes and properties; trusted
  mousedown / mouseup / click (a cancelled click doesn't follow its link, press its button or
  toggle its checkbox; labels click their control), keydown / keypress / keyup (a cancelled
  key doesn't type or scroll), input and change, focus / blur / focusin / focusout (and
  change when a changed field is left), submit (cancelling it stops the submission), reset,
  invalid, `DOMContentLoaded`, `load` and `readystatechange`; `Event`, `CustomEvent` and
  the UI event constructors for `dispatchEvent`.
* **Timers and the window**: `setTimeout` / `setInterval` (the spec's nesting clamp),
  `requestAnimationFrame` (run just before a frame is drawn, at the display's rate while a
  page asks), `queueMicrotask` and promises (drained after each task); `console.*` (each line
  on the serial port as `[browser] console LEVEL: ...`, and the last 400 per tab in the
  console panel, Cmd-Alt-J or the menu); `location` (read, assign, replace, reload),
  `history` (back, forward, go, pushState / replaceState for the address bar), `navigator`,
  `screen`, `localStorage` per origin (kept in `~/Library/Browser/storage`, 5 MB each) and
  `sessionStorage` per tab, `document.cookie` (without HttpOnly cookies), `alert` / `confirm`
  / `prompt` as dialogs over the page, `window.open` as a new tab.
* **Safety**: scripts run inside the Browser's sandbox (no files, no other tabs, no other
  origins' storage). Each tab has its own realm with a 64 MB heap; a script that goes over
  it is stopped. A script that runs for two seconds without a break brings up "This page is
  slow ... Stop it?" over the page: Stop (or Esc) ends the page's scripts for good (its
  timers and listeners go, the page stays as it is), Wait gives it ten more seconds. Pages
  that rebuild their content over and over stay in bounded memory: nodes no script refers to
  are given back when they leave the tree. Background tabs run their timers once a second.
* **Network requests** (stage 2a): `fetch` with `Request`, `Response` and `Headers` (guards
  and all), `XMLHttpRequest`, `navigator.sendBeacon`, `AbortController` / `AbortSignal`
  (`abort(reason)`, `timeout(ms)`, `any([...])`), `URL` and `URLSearchParams`, `Blob`, `File`,
  `FileReader`, `FormData` (also from a `<form>`), `ReadableStream` (a default reader over a
  body), `DOMException` and `ProgressEvent`, and `window.onerror` / `error` and
  `unhandledrejection` events (`ErrorEvent`, `PromiseRejectionEvent`). The classes are written
  in script (`webengine/src/script/js/*.js`) and compiled the first time a page touches one
  of the names (until then they are accessors on `window`, so pages that never use them pay
  nothing); `script/net.rs` is the glue and `script/netbytes.rs` holds byte buffers outside the
  script heap (a body or a Blob's content never goes through script as numbers; an
  `ArrayBuffer` is made only when the page asks for one). The engine does no I/O: a request
  comes out of `Document::take_net` as a `NetRequest`, and the app's answer goes back with
  `Document::net_event` (head, progress, body, or a failure), one task of the page's event
  loop each. Choices:
  * `fetch` resolves when the head arrives (a body that is still coming makes `text()` wait,
    and an abort rejects it); `response.body` is a `ReadableStream` that gives the whole body
    as one `Uint8Array` chunk (no streaming of a request body: `new Request(url, {body:
    stream})` is a TypeError; no `tee` across a locked stream, no `pipeTo`); a failed request
    rejects with `TypeError: Failed to fetch` (the reason is in `error.cause` and, with the
    policy's wording, on the console; the page can't tell CORS from a dead server, as on the
    web).
  * **Synchronous XHR is refused**: `open(method, url, false)` throws `InvalidAccessError` and
    the console says why (a blocking request would freeze the tab, and pages run on one
    thread). `responseType = "document"` is accepted and gives `null` (no HTML parser for
    scripts yet); `responseText` fills in when the body is complete (`readyState` 3 and
    `progress` events still come, with `lengthComputable` when the server sent a length).
  * `data:` and `blob:` (from `URL.createObjectURL`) addresses are answered in the page. A
    request body is at most 32 MB before the policy's own 4 MiB limit applies; at most 256
    requests are outstanding per page.
  * The rules (same-origin policy, CORS, preflight and its cache, credentials, cookies,
    redirects hop by hop, mixed content, the private-network rule with the address a page was
    fetched from, limits) are `browserkit::netpolicy`'s; the bindings only shape the API and
    drop what a script may never set, and the app applies the rules again to everything.
* **Restyling**: changes are batched until the next frame; only the changed subtrees are
  restyled (a change's parent's subtree, so sibling selectors see it), and the page is laid
  out again once. Hit testing, the focus and form state follow the new tree.

## Inside

* `net.rs`: every page, image, stylesheet and download is a job on its own thread (512 KiB
  stacks for the TLS handshake, at most six at once). A job runs `milkweb::http::fetch` with
  the Browser's hooks: the cookie jar asked and fed on every hop, progress, Stop, and a page
  that turns out to be a file becomes a download, written by the job itself.
* `web.rs`: the engine's `Document` behind the calls the app makes; `draw.rs` paints the
  chrome and, through `browserkit::paint`, the display list and the form controls (inside the
  boxes the engine painted); `tab.rs` and `store.rs` (the profile files, written to a
  temporary name and renamed).
* `request.rs` and `user/scriptnet`: a page's script requests. `scriptnet::run` takes one
  `NetRequest` through `netpolicy::Exchange` and milkweb hop by hop (redirects are followed by
  hand so each hop is judged; a preflight is an OPTIONS before the request; the head is
  announced as soon as it is known; every resolved address is checked before connecting, so a
  public page can't reach the local network through a name that points there). `net.rs` runs
  each one on a worker thread (`Kind::Request`, up to eight at once besides the six for pages,
  so a long poll can't keep a page from loading), `request.rs` keeps the tab's limits and
  queue (`netpolicy::Limiter`, cleared when the tab shows another page, and their jobs
  cancelled), delivers the answers to the page as tasks, logs each request on the serial line
  and in the console panel's Network list (click its label), and the tab shows a small dot
  while requests are outstanding. A request belongs to the page that made it: navigating or
  closing the tab cancels it and drops its answers.
* `js.rs`: the pages' scripts: the `ScriptHost` (clock, cookies, storage, dialogs, the slow
  bar), running each tab's scripts and animation frames, what scripts ask for, the console.
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
  panic) and zoom, and the script networking rules (`netpolicy`: origins, address parsing,
  headers and their guards, CORS and preflight, redirects, cookies, mixed content, limits; table
  tests (well over a thousand rows) plus fuzzing of addresses, headers and whole exchanges against
  a hostile server, `NETPOLICY_FUZZ=N` for longer runs).
* `scripts/browser-test.sh` (default set, about two minutes): a VM against
  `scripts/browser_test_servers.py` on the Mac's 127.0.0.1 only (a DNS server for a made-up zone
  and the test pages). It checks the client protocol, that `/dev/wm` lists the window at once,
  a styled page and its stylesheet, a Greek and Cyrillic page, JPEG, GIF and WebP pictures (the
  animated ones play, and stand still in a background tab),
  scrolling, a fragment, find, zoom, back, a keyboard-driven GET form, a POST sign-in whose
  cookie comes back, third-party cookies refused, tabs, Cmd-click, `open URL`, bookmarks,
  downloads, error pages and the sandbox, from the serial log, the server's log and
  screenshots.
* `scripts/js-test.sh` (default set, about a minute and a half): a VM against
  `scripts/js_test_server.py` (127.0.0.1 only) serving the pages in `tests/js`: a tabs
  widget, a todo list built by script, form validation that blocks the submit, timers and
  microtasks, a requestAnimationFrame animation, innerHTML, the script order (blocking,
  defer, a module in order with them, document.write, DOMContentLoaded, load), storage and cookies, a runaway
  `while (true)` stopped from the slow bar, and the JavaScript switch, from the console
  lines, the server's log and screenshots.
* `scripts/fetch-test.sh` (default set, about a minute): a VM against
  `scripts/fetch_test_servers.py` (a DNS zone with `www.milk.test`, `api.milk.test` (the same
  site, another origin) and `other.test` (another site), and a web server that sets and checks
  CORS headers; 127.0.0.1 only) and the pages in `tests/fetch`: same-origin `fetch` (bodies of
  every kind, redirects, errors, abort and timeout, a response over the limit, cookies hidden
  and sent, ten in parallel, `sendBeacon`, `unhandledrejection`), `XMLHttpRequest` (event
  order, `responseType`s, progress, upload, timeout, abort, errors, synchronous XHR refused),
  cross-origin `fetch` (Allow-Origin, exposed headers, preflight and its cache, a denied
  preflight, credentials with cookies, no third-party cookies, `no-cors`, redirects), and a
  page that loads JSON and builds itself with script (screenshots), with the Network list.
  Mixed content (an https page asking for http) is covered by `scriptnet-unit` only: the VM
  has no https test server.
* `scripts/modules-test.sh` (default set, about three minutes): a VM against
  `scripts/modules_test_servers.py` (the same kind of zone as fetch-test: `www.milk.test`,
  `api.milk.test`, `other.test`; 127.0.0.1 only) and the pages in `tests/modules`: modules
  importing each other, a cycle, each module run once, `nomodule`, deferred and async order,
  top-level `await` with siblings, `import()` from a classic script, an import map (bare
  specifiers, a prefix, a scope, a blocked name, a second map and a late one ignored) and a
  page without one, a module blocked without CORS headers and allowed with them (also through
  redirects), cookies for same-origin, `anonymous` and `use-credentials`, wrong MIME types,
  404 and 500 and a bad dependency (error events and console errors, the other modules
  unharmed), JSON modules, a small app built from modules (screenshots), the depth and count
  limits, a module abandoned mid-download by going to another page, and the JavaScript switch.
* `scripts/scriptnet-test.sh` (`scriptnet-unit`, smoke): `user/scriptnet` against a fake web
  and the real policy: methods and bodies on the wire, CORS and preflight, credentials,
  redirects hop by hop, cookies, the private-network and rebinding refusals, mixed content,
  size and time limits, cancelling, and the strict checks for module requests (a success
  status and a JavaScript type, in mode cors).
* `scripts/webengine-test.sh` also covers modules: import map parsing and resolution (table
  tests), the module map and fetch graph over a fake engine (dedup, cycles, limits, failures,
  a fuzz of random events), the loader's ordering, and whole pages over a fake network
  (`src/tests/modules.rs`: ordering, errors, `import()`, JSON, preloads, limits, hostile
  maps), and the network bindings (`src/tests/nettest.rs`, over a
  fake network: every class's behaviour, request shapes, answers out of order or for nobody,
  limits, a fuzz of the parsers from script) and the DOM changes (fragment parsing, serializing,
  incremental restyling checked against a fresh parse and a full restyle, form state across
  tree changes) and the bindings (`src/tests/script.rs`: the DOM, events, forms, timers,
  storage, location, focus, bounded memory under churn; `src/tests/scripts.rs`: script
  text against fixture pages).
* `scripts/web-test.sh` and `scripts/tls-interop.sh` open pages by host name and over https
  (a good certificate and one for another site) through `open URL`.

## Known gaps

* Font sizes snap to 13, 26 and 52 px (the baked faces), so `font-size: 16px` text is 13 px;
  there are no real italics or serif faces; CJK and other scripts no family has show as boxes.
* JavaScript has no WebSockets, canvas, workers or `ReadableStream` request bodies;
  `fetch` has no HTTP cache (`cache` modes only set headers) and no `integrity` check; hosts
  with non-ASCII names aren't looked up (no IDNA); `URL` is a good WHATWG subset, not the whole
  parser. MutationObserver callbacks never run (the observers exist and never call back);
  layout after a change is a whole-page layout (restyling is per subtree); `:hover` and `:focus` styles
  don't apply.
* No `<iframe>`, no HTTP cache or HTTP/2, no file upload in forms, no
  `<video>` / `<audio>`; arithmetic-coded or 12-bit JPEG, colour profiles.
* "Site" for cookies is the last two labels of the host (with a short list of two-label
  suffixes like `co.uk`), not the full public suffix list.
* No hover: client windows get pointer moves only while a button is down, so links don't
  show their address before a click (and there are no tooltips); links have no pressed
  colour yet.
* Closing the window can't be refused, so a download still running is lost when the window
  closes; Cmd-W closes the window, not the tab.
* A textarea's caret is always at the end of its text.
