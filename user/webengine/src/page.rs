//! A parsed page: ties the stages together and keeps their results (DOM, stylesheets,
//! images, form state, the last layout and display list) so relayout and restyle happen only
//! when something changed.

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use crate::api::*;
use crate::atoms::tag;
use crate::css::sheet::{self, MediaEnv, MediaList, Rule, Sheet, SheetLimits};
use crate::css::tokenizer::tokenize;
use crate::dom::{Dom, NONE, NodeData, NodeId};
use crate::forms::{self, Forms};
use crate::layout::build::{ImageLookup, ImageState};
use crate::layout::{self, AU, Cx};
use crate::paint::{self, Painted};
use crate::style::cascade::{RuleSet, StyleCx, Styles, style_document};
use crate::style::props::ParseCx;

enum LinkState {
    Pending,
    Loaded(Rc<Sheet>),
    Failed,
}

enum Slot {
    /// A `<style>` element (or the UA sheet).
    Ready {
        sheet: Rc<Sheet>,
        media: MediaList,
        ua: bool,
    },
    /// `<link rel=stylesheet>`.
    Linked {
        url: String,
        media: MediaList,
        state: LinkState,
    },
}

pub struct Page {
    url: String,
    base: String,
    pub(crate) dom: Dom,
    truncated: bool,
    limits: Limits,
    slots: Vec<Slot>,
    imports: BTreeMap<String, LinkState>,
    images: Vec<ImageState>,
    image_ids: BTreeMap<String, ImageId>,
    img_nodes: BTreeMap<NodeId, ImageId>,
    forms: Forms,
    styles: Option<Styles>,
    rules: Option<RuleSet>,
    rules_env: Option<MediaEnv>,
    sheets_changed: bool,
    layout_dirty: bool,
    last_vp: Option<Viewport>,
    painted: Option<Painted>,
    empty: DisplayList,
    boxes: usize,
    rules_count: usize,
    /// The dark UA defaults (ua-dark.css), and whether the rules now use them.
    ua_dark_sheet: Option<Rc<Sheet>>,
    ua_dark: bool,
    /// `<meta name=color-scheme>`: bit 1 light, 2 dark, 4 only.
    meta_scheme: u8,
    /// The UA sheet, parsed once.
    ua_sheet: Option<Rc<Sheet>>,
    /// `<style>` sheets by element and text (not parsed again when a script changes others).
    style_cache: Vec<(NodeId, String, Rc<Sheet>)>,
    /// The scripting flag the page was parsed with (scripts on: `<noscript>` is raw text).
    pub(crate) scripting: bool,
    /// The element with the keyboard (NONE: the page itself).
    pub(crate) focus: NodeId,
    /// Subtrees to restyle at the next layout (scripts changed them).
    pending_restyle: Vec<NodeId>,
    /// Restyles done incrementally and in full since parsing (for tests and the debug line).
    pub(crate) restyles: (u32, u32),
    /// No author styles (`<style>`, a stylesheet link, `style=`) and no colour attributes
    /// (bgcolor, text, link, ...): the page leaves its colours to the browser entirely.
    plain: bool,
}

/// The canvas when the page doesn't paint one (SPEC "Type": milk, or Chocolate milk).
pub const CANVAS_LIGHT: Color = 0xFFFF_FDF8;
pub const CANVAS_DARK: Color = 0xFF2E_2520;

fn decode_css(b: &[u8]) -> String {
    let b = b.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(b);
    match core::str::from_utf8(b) {
        Ok(s) => String::from(s),
        Err(_) => b.iter().map(|&x| crate::charset::windows1252(x)).collect(),
    }
}

fn escape_html(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 32);
    for c in s.chars() {
        match c {
            '<' => o.push_str("&lt;"),
            '&' => o.push_str("&amp;"),
            c => o.push(c),
        }
    }
    o
}

fn is_text_plain(ct: Option<&str>) -> bool {
    ct.is_some_and(|c| {
        let m = c
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        m == "text/plain"
            || m == "text/css"
            || m == "application/javascript"
            || m == "text/javascript"
            || m == "application/json"
    })
}

impl Page {
    pub fn parse(bytes: &[u8], url: &str, ct: Option<&str>, limits: Limits) -> Page {
        Page::parse_scripting(bytes, url, ct, limits, false)
    }

    pub fn parse_scripting(
        bytes: &[u8],
        url: &str,
        ct: Option<&str>,
        limits: Limits,
        scripting: bool,
    ) -> Page {
        let mut truncated = false;
        let bytes = if bytes.len() > limits.max_input {
            truncated = true;
            &bytes[..limits.max_input]
        } else {
            bytes
        };
        let (text, _) = crate::charset::decode(bytes, ct);
        if is_text_plain(ct) {
            let html = alloc::format!(
                "<!DOCTYPE html><html><head></head><body><pre>{}</pre></body></html>",
                escape_html(&text)
            );
            let mut p = Page::parse_str_with(&html, url, limits, false);
            p.truncated |= truncated;
            return p;
        }
        let mut p = Page::parse_str_with(&text, url, limits, scripting);
        p.truncated |= truncated;
        p
    }

    pub fn parse_str(html: &str, url: &str, limits: Limits) -> Page {
        Page::parse_str_with(html, url, limits, false)
    }

    pub fn parse_str_with(html: &str, url: &str, limits: Limits, scripting: bool) -> Page {
        let html = if html.len() > limits.max_input {
            let mut e = limits.max_input;
            while !html.is_char_boundary(e) {
                e -= 1;
            }
            &html[..e]
        } else {
            html
        };
        let parsed = crate::html::parse_document_with(html, &limits, scripting);
        let mut p = Page {
            url: String::from(url),
            base: String::from(url),
            dom: parsed.dom,
            truncated: parsed.truncated || html.len() >= limits.max_input,
            limits,
            slots: Vec::new(),
            imports: BTreeMap::new(),
            images: Vec::new(),
            image_ids: BTreeMap::new(),
            img_nodes: BTreeMap::new(),
            forms: Forms::empty(),
            styles: None,
            rules: None,
            rules_env: None,
            sheets_changed: true,
            layout_dirty: true,
            last_vp: None,
            painted: None,
            empty: DisplayList::default(),
            boxes: 0,
            rules_count: 0,
            ua_dark_sheet: None,
            ua_dark: false,
            meta_scheme: 0,
            plain: true,
            ua_sheet: None,
            style_cache: Vec::new(),
            scripting,
            focus: NONE,
            pending_restyle: Vec::new(),
            restyles: (0, 0),
        };
        p.scan();
        p.dom.track = true;
        p
    }

    /// Walk the DOM: base URL, stylesheets, images, form controls.
    fn scan(&mut self) {
        // <base href> (HTML §4.2.3: the first one with an href).
        let mut n = 0;
        self.base = self.url.clone();
        while n != NONE {
            if let Some(e) = self.dom.element(n)
                && e.is(tag::BASE)
                && let Some(h) = e.attr("href")
            {
                self.base = crate::url::resolve(&self.url, h);
                break;
            }
            n = self.dom.next_in(n, 0);
        }
        let dark = self.parse_sheet(crate::style::UA_DARK_CSS, "about:ua");
        self.ua_dark_sheet = Some(Rc::new(dark));
        // With scripts on, <noscript> holds raw text that isn't shown.
        let ua = if self.scripting {
            let css = alloc::format!("{}\nnoscript {{ display: none }}\n", crate::style::UA_CSS);
            self.parse_sheet(&css, "about:ua")
        } else {
            self.parse_sheet(crate::style::UA_CSS, "about:ua")
        };
        self.ua_sheet = Some(Rc::new(ua));
        self.scan_sheets();
        self.scan_images();
        self.forms = forms::collect(&self.dom, 100_000);
        self.attach_form_images();
    }

    fn attach_form_images(&mut self) {
        for (i, &node) in self.forms.nodes.iter().enumerate() {
            self.forms.controls[i].image = self.img_nodes.get(&node).copied();
        }
    }

    /// The stylesheets in tree order (`<style>`, `<link rel=stylesheet>`), plus whether the
    /// page sets colours of its own and its `<meta name=color-scheme>`. Called again when a
    /// script adds, removes or changes one: linked sheets already fetched (or failed) are
    /// kept by URL, `<style>` sheets whose text is the same are not parsed again.
    fn scan_sheets(&mut self) {
        let mut linked: BTreeMap<String, LinkState> = BTreeMap::new();
        let mut styles: Vec<(NodeId, String, Rc<Sheet>)> = core::mem::take(&mut self.style_cache);
        for s in self.slots.drain(..) {
            if let Slot::Linked { url, state, .. } = s
                && !matches!(state, LinkState::Pending)
            {
                linked.insert(url, state);
            }
        }
        self.rules_count = 0;
        if let Some(ua) = &self.ua_sheet {
            self.rules_count += sheet::count_rules(&ua.rules);
            self.slots.push(Slot::Ready {
                sheet: ua.clone(),
                media: MediaList::default(),
                ua: true,
            });
        }
        self.plain = true;
        self.meta_scheme = 0;
        let mut n = 0;
        while n != NONE {
            let Some(e) = self.dom.element(n) else {
                n = self.dom.next_in(n, 0);
                continue;
            };
            if e.is(tag::TEMPLATE) || e.ns != crate::dom::Ns::Html {
                n = self.dom.next_skipping_children(n, 0);
                continue;
            }
            let name = e.name;
            // Does the page set any colours (or styles) of its own?
            const COLOUR_ATTRS: [&str; 8] = [
                "style",
                "bgcolor",
                "text",
                "link",
                "vlink",
                "alink",
                "color",
                "background",
            ];
            if COLOUR_ATTRS.iter().any(|a| e.has_attr(a)) {
                self.plain = false;
            }
            if e.is(tag::META)
                && e.attr("name")
                    .is_some_and(|v| v.trim().eq_ignore_ascii_case("color-scheme"))
                && let Some(c) = e.attr("content")
            {
                for w in c.split(crate::dom::is_html_space) {
                    match w.to_ascii_lowercase().as_str() {
                        "light" => self.meta_scheme |= 1,
                        "dark" => self.meta_scheme |= 2,
                        "only" => self.meta_scheme |= 4,
                        _ => {}
                    }
                }
            }
            match name {
                tag::STYLE => {
                    self.plain = false;
                    let media = media_attr(e.attr("media"));
                    let css = self.dom.text_content(n);
                    if self.slots.len() < self.limits.max_stylesheets {
                        let sheet = match styles.iter().position(|(sn, t, _)| *sn == n && *t == css)
                        {
                            Some(i) => {
                                let (_, _, sh) = styles.swap_remove(i);
                                self.rules_count += sheet::count_rules(&sh.rules);
                                sh
                            }
                            None => {
                                let base = self.base.clone();
                                let s = self.parse_sheet(&css, &base);
                                self.queue_imports(&s);
                                Rc::new(s)
                            }
                        };
                        self.style_cache.push((n, css, sheet.clone()));
                        self.slots.push(Slot::Ready {
                            sheet,
                            media,
                            ua: false,
                        });
                    }
                }
                tag::LINK => {
                    let rel = e.attr("rel").unwrap_or("").to_ascii_lowercase();
                    let rels: Vec<&str> = rel.split(crate::dom::is_html_space).collect();
                    if rels.contains(&"stylesheet")
                        && !rels.contains(&"alternate")
                        && !e.has_attr("disabled")
                        && let Some(h) = e.attr("href")
                        && !h.trim().is_empty()
                        && self.slots.len() < self.limits.max_stylesheets
                    {
                        let url = crate::url::resolve(&self.base, h);
                        let media = media_attr(e.attr("media"));
                        self.plain = false;
                        let state = match linked.get(&url) {
                            Some(LinkState::Loaded(sh)) => {
                                self.rules_count += sheet::count_rules(&sh.rules);
                                LinkState::Loaded(sh.clone())
                            }
                            Some(_) => LinkState::Failed,
                            None => LinkState::Pending,
                        };
                        self.slots.push(Slot::Linked { url, media, state });
                    }
                }
                _ => {}
            }
            n = self.dom.next_in(n, 0);
        }
        self.sheets_changed = true;
        self.layout_dirty = true;
    }

    /// Which `<img>` / `<input type=image>` shows which image.
    fn scan_images(&mut self) {
        self.img_nodes.clear();
        let mut n = 0;
        while n != NONE {
            let Some(e) = self.dom.element(n) else {
                n = self.dom.next_in(n, 0);
                continue;
            };
            if e.is(tag::TEMPLATE) || e.ns != crate::dom::Ns::Html {
                n = self.dom.next_skipping_children(n, 0);
                continue;
            }
            let is_img_input = e.is(tag::INPUT)
                && e.attr("type")
                    .is_some_and(|t| t.trim().eq_ignore_ascii_case("image"));
            if e.is(tag::IMG) || is_img_input {
                let src = e.attr("src").map(String::from).or_else(|| {
                    // No src: the first srcset candidate.
                    e.attr("srcset")
                        .and_then(|s| s.split(',').next())
                        .and_then(|c| c.split_whitespace().next())
                        .map(String::from)
                });
                if let Some(src) = src.filter(|s| !s.trim().is_empty()) {
                    let url = crate::url::resolve(&self.base, &src);
                    if let Some(id) = self.image_id(&url) {
                        self.img_nodes.insert(n, id);
                    }
                }
            }
            n = self.dom.next_in(n, 0);
        }
    }

    fn parse_sheet(&mut self, css: &str, base: &str) -> Sheet {
        let base = String::from(base);
        let resolve = move |h: &str| crate::url::resolve(&base, h);
        let cx = ParseCx { resolve: &resolve };
        let left = self.limits.max_rules.saturating_sub(self.rules_count);
        let s = sheet::parse(
            css,
            &cx,
            &mut self.dom.atoms,
            &SheetLimits {
                max_tokens: 4 << 20,
                max_rules: left,
            },
        );
        self.rules_count += sheet::count_rules(&s.rules);
        s
    }

    fn queue_imports(&mut self, s: &Sheet) {
        fn walk(rules: &[Rule], out: &mut Vec<String>) {
            for r in rules {
                match r {
                    Rule::Import { url, .. } => out.push(url.clone()),
                    Rule::Media { rules, .. } => walk(rules, out),
                    _ => {}
                }
            }
        }
        let mut urls = Vec::new();
        walk(&s.rules, &mut urls);
        for u in urls {
            if self.imports.len() + self.slots.len() >= self.limits.max_stylesheets {
                break;
            }
            self.imports.entry(u).or_insert(LinkState::Pending);
        }
    }

    fn image_id(&mut self, url: &str) -> Option<ImageId> {
        if let Some(id) = self.image_ids.get(url) {
            return Some(*id);
        }
        if self.images.len() >= self.limits.max_images {
            return None;
        }
        let id = ImageId(self.images.len() as u32);
        self.images.push(ImageState {
            url: String::from(url),
            size: None,
            failed: false,
        });
        self.image_ids.insert(String::from(url), id);
        Some(id)
    }

    pub fn title(&self) -> String {
        let t = self.dom.find_tag(tag::TITLE);
        if t == NONE {
            return String::new();
        }
        let raw = self.dom.text_content(t);
        raw.split(crate::dom::is_html_space)
            .filter(|w| !w.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    pub fn resolve(&self, href: &str) -> String {
        crate::url::resolve(&self.base, href)
    }

    pub fn favicon(&self) -> Option<String> {
        let mut n = 0;
        while n != NONE {
            if let Some(e) = self.dom.element(n)
                && e.is(tag::LINK)
            {
                let rel = e.attr("rel").unwrap_or("").to_ascii_lowercase();
                if rel.split(crate::dom::is_html_space).any(|r| r == "icon")
                    && let Some(h) = e.attr("href")
                {
                    return Some(self.resolve(h));
                }
            }
            n = self.dom.next_in(n, 0);
        }
        None
    }

    pub fn meta_refresh(&self) -> Option<(u32, Option<String>)> {
        let mut n = 0;
        while n != NONE {
            if let Some(e) = self.dom.element(n)
                && e.is(tag::META)
                && e.attr("http-equiv")
                    .is_some_and(|v| v.trim().eq_ignore_ascii_case("refresh"))
                && let Some(c) = e.attr("content")
            {
                // HTML §4.2.5.3 "shared declarative refresh steps", simplified.
                let c = c.trim();
                let digits: String = c.chars().take_while(|ch| ch.is_ascii_digit()).collect();
                let secs: u32 = digits.parse().unwrap_or(0);
                let rest = c[digits.len()..]
                    .trim_start_matches(|ch: char| ch.is_ascii_digit() || ch == '.');
                let rest = rest
                    .trim_start_matches(|ch: char| ch == ';' || ch == ',' || ch.is_whitespace());
                let url = if rest.len() >= 3 && rest[..3].eq_ignore_ascii_case("url") {
                    let r = rest[3..].trim_start();
                    let r = r.strip_prefix('=').unwrap_or(r).trim();
                    let r = r.trim_matches(|ch| ch == '\'' || ch == '"');
                    if r.is_empty() {
                        None
                    } else {
                        Some(self.resolve(r))
                    }
                } else if !rest.is_empty() {
                    Some(self.resolve(rest.trim_matches(|ch| ch == '\'' || ch == '"')))
                } else {
                    None
                };
                return Some((secs, url));
            }
            n = self.dom.next_in(n, 0);
        }
        None
    }

    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// Stylesheets first; then images (`<img>` ones always, background images once styles
    /// have been computed, which [`Page::layout`] and [`Page::load_resources`] do).
    pub fn pending_resources(&self) -> Vec<Resource> {
        let mut out = Vec::new();
        for s in &self.slots {
            if let Slot::Linked {
                url,
                state: LinkState::Pending,
                ..
            } = s
            {
                out.push(Resource {
                    kind: ResourceKind::Stylesheet,
                    url: url.clone(),
                });
            }
        }
        for (u, st) in &self.imports {
            if matches!(st, LinkState::Pending) {
                out.push(Resource {
                    kind: ResourceKind::Stylesheet,
                    url: u.clone(),
                });
            }
        }
        for (i, im) in self.images.iter().enumerate() {
            if im.size.is_none() && !im.failed {
                out.push(Resource {
                    kind: ResourceKind::Image(ImageId(i as u32)),
                    url: im.url.clone(),
                });
            }
        }
        out
    }

    pub fn supply_stylesheet(&mut self, url: &str, body: Option<&[u8]>) {
        let parsed = body.map(|b| {
            let css = decode_css(b);
            let s = self.parse_sheet(&css, url);
            self.queue_imports(&s);
            Rc::new(s)
        });
        let mut any = false;
        for s in self.slots.iter_mut() {
            if let Slot::Linked { url: u, state, .. } = s
                && u == url
                && matches!(state, LinkState::Pending)
            {
                *state = match &parsed {
                    Some(p) => LinkState::Loaded(p.clone()),
                    None => LinkState::Failed,
                };
                any = true;
            }
        }
        if let Some(st) = self.imports.get_mut(url)
            && matches!(st, LinkState::Pending)
        {
            *st = match &parsed {
                Some(p) => LinkState::Loaded(p.clone()),
                None => LinkState::Failed,
            };
            any = true;
        }
        if any {
            self.sheets_changed = true;
            self.layout_dirty = true;
        }
    }

    pub fn supply_image(&mut self, id: ImageId, size: Option<(u32, u32)>) {
        if let Some(im) = self.images.get_mut(id.0 as usize) {
            match size {
                Some((w, h)) => {
                    im.size = Some((w.min(1 << 20), h.min(1 << 20)));
                    im.failed = false;
                }
                None => im.failed = true,
            }
            self.layout_dirty = true;
        }
    }

    pub fn image_url(&self, id: ImageId) -> &str {
        self.images
            .get(id.0 as usize)
            .map(|i| i.url.as_str())
            .unwrap_or("")
    }

    pub fn load_resources(&mut self, f: &mut dyn Fetcher) {
        for _ in 0..16 {
            let mut pending = self.pending_resources();
            if !pending.iter().any(|r| r.kind == ResourceKind::Stylesheet) {
                // All sheets are in: style now so background images show up as pending.
                let vp = self.last_vp.unwrap_or(Viewport::new(1024, 768));
                self.ensure_styled(&vp);
                pending = self.pending_resources();
            }
            if pending.is_empty() {
                return;
            }
            for r in pending {
                match r.kind {
                    ResourceKind::Stylesheet => {
                        let body = f.stylesheet(&r.url);
                        self.supply_stylesheet(&r.url, body.as_deref());
                    }
                    ResourceKind::Image(id) => {
                        let size = f.image(&r.url, id);
                        self.supply_image(id, size);
                    }
                }
            }
        }
    }

    /// Rebuild rules and restyle if the sheets or the media environment changed, with the
    /// dark UA defaults when they apply ([`Page::want_dark`]: the root's `color-scheme` is
    /// only known after styling, so a page that turns out to want them is styled again).
    fn ensure_styled(&mut self, vp: &Viewport) {
        for _ in 0..2 {
            let want = self.want_dark(vp);
            let force = want != self.ua_dark;
            self.ua_dark = want;
            self.style_pass(vp, force);
            if self.want_dark(vp) == self.ua_dark {
                break;
            }
        }
    }

    /// The dark UA defaults: only when the desktop is dark, and the page either says it can be
    /// dark (`color-scheme` on the root, `<meta name=color-scheme>`) or leaves every colour to
    /// the browser. A page that sets some colours but no scheme stays light (dark text on a
    /// dark canvas would be worse than a light page in dark chrome).
    fn want_dark(&self, vp: &Viewport) -> bool {
        if !vp.dark {
            return false;
        }
        let root = self.dom.document_element();
        let css = self
            .styles
            .as_ref()
            .and_then(|st| st.of.get(root as usize))
            .and_then(|s| s.as_ref())
            .map_or(0, |s| s.color_scheme);
        let scheme = self.meta_scheme | css;
        scheme & 2 != 0 || (scheme == 0 && self.plain)
    }

    fn style_pass(&mut self, vp: &Viewport, force: bool) {
        let zoom = vp.zoom.clamp(25, 500) as f32 / 100.0;
        let env = MediaEnv {
            width: vp.width.max(1) as f32 / zoom,
            height: vp.height.max(1) as f32 / zoom,
            dark: vp.dark,
        };
        let env_changed = self.rules_env != Some(env);
        let need_rules = force
            || self.rules.is_none()
            || self.sheets_changed
            || (env_changed && self.rules.as_ref().is_some_and(|r| r.media_rules));
        // vw/vh units depend on the viewport even without media rules.
        let need_style = need_rules || self.styles.is_none() || env_changed;
        if need_rules {
            let mut list: Vec<(Rc<Sheet>, bool)> = Vec::new();
            for s in &self.slots {
                match s {
                    Slot::Ready { sheet, media, ua } if media.matches(&env) => {
                        list.push((sheet.clone(), *ua));
                        if *ua
                            && self.ua_dark
                            && let Some(d) = &self.ua_dark_sheet
                        {
                            list.push((d.clone(), true));
                        }
                    }
                    Slot::Linked {
                        media,
                        state: LinkState::Loaded(sheet),
                        ..
                    } if media.matches(&env) => list.push((sheet.clone(), false)),
                    _ => {}
                }
            }
            let mut imports = BTreeMap::new();
            for (u, st) in &self.imports {
                if let LinkState::Loaded(s) = st {
                    imports.insert(u.clone(), s.clone());
                }
            }
            let mut rs = RuleSet::build(&list, &imports, &env);
            if self.slots.iter().any(|s| match s {
                Slot::Ready { media, .. } | Slot::Linked { media, .. } => !media.is_empty(),
            }) {
                rs.media_rules = true;
            }
            self.rules = Some(rs);
            self.sheets_changed = false;
        }
        self.rules_env = Some(env);
        let incremental = !need_style && !self.pending_restyle.is_empty();
        if need_style || incremental {
            let base = self.base.clone();
            let resolve = move |h: &str| crate::url::resolve(&base, h);
            let images = &mut self.images;
            let ids = &mut self.image_ids;
            let max_images = self.limits.max_images;
            let mut image = |url: &str| -> ImageId {
                if let Some(id) = ids.get(url) {
                    return *id;
                }
                if images.len() >= max_images {
                    return ImageId(u32::MAX);
                }
                let id = ImageId(images.len() as u32);
                images.push(ImageState {
                    url: String::from(url),
                    size: None,
                    failed: false,
                });
                ids.insert(String::from(url), id);
                id
            };
            let mut scx = StyleCx {
                env,
                resolve: &resolve,
                image: &mut image,
            };
            let rules = self.rules.as_ref().unwrap();
            let roots = core::mem::take(&mut self.pending_restyle);
            let done = incremental
                && self.styles.as_mut().is_some_and(|st| {
                    crate::style::cascade::restyle_subtrees(&self.dom, rules, &mut scx, st, &roots)
                });
            if done {
                self.restyles.0 += 1;
            } else {
                self.styles = Some(style_document(&self.dom, rules, &mut scx));
                self.restyles.1 += 1;
            }
            self.layout_dirty = true;
        }
        self.pending_restyle.clear();
    }

    /// Catch up with what scripts changed in the DOM since the last time: stylesheets,
    /// images and form controls looked at again where that's needed, the changed subtrees
    /// marked for restyling, and the next layout made to happen.
    pub(crate) fn flush(&mut self) {
        self.sync_forms();
        self.sync_checked();
        let ch = self.dom.take_changes();
        if !(ch.any || ch.tree || ch.text || ch.sheets || ch.forms || ch.images) {
            return;
        }
        if ch.sheets {
            self.scan_sheets();
        }
        if ch.images {
            self.scan_images();
        }
        if ch.forms {
            self.forms = forms::recollect(&self.dom, &self.forms, 100_000);
            self.attach_form_images();
        } else if ch.images {
            self.attach_form_images();
        }
        if self.focus != NONE && !self.dom.is_connected(self.focus) {
            self.focus = NONE;
        }
        if ch.restyle_all {
            self.styles = None;
            self.pending_restyle.clear();
        } else {
            for r in ch.restyle {
                if !self.pending_restyle.contains(&r) {
                    self.pending_restyle.push(r);
                }
            }
            if self.pending_restyle.len() > crate::dom::MAX_RESTYLE_ROOTS {
                self.styles = None;
                self.pending_restyle.clear();
            }
        }
        self.layout_dirty = true;
    }

    /// Keep what `:checked` matches in step with the form state (the user's clicks, scripts),
    /// restyling around the controls that changed.
    pub(crate) fn sync_checked(&mut self) {
        use alloc::collections::BTreeSet;
        let mut now: BTreeSet<NodeId> = BTreeSet::new();
        for (i, c) in self.forms.controls.iter().enumerate() {
            match c.kind {
                ControlKind::Checkbox | ControlKind::Radio if c.checked => {
                    now.insert(self.forms.nodes[i]);
                }
                ControlKind::Select => {
                    for (k, o) in c.options.iter().enumerate() {
                        if o.selected
                            && let Some(&n) = self.forms.option_nodes[i].get(k)
                        {
                            now.insert(n);
                        }
                    }
                }
                _ => {}
            }
        }
        let old = match self.dom.checked.take() {
            Some(s) => s,
            None => {
                // What the attributes said (what styling used so far).
                let mut s = BTreeSet::new();
                let mut n = 0;
                while n != NONE {
                    if let Some(e) = self.dom.element(n)
                        && ((e.is(tag::INPUT) && e.has_attr("checked"))
                            || (e.is(tag::OPTION) && e.has_attr("selected")))
                    {
                        s.insert(n);
                    }
                    n = self.dom.next_in(n, 0);
                }
                s
            }
        };
        let changed: Vec<NodeId> = old.symmetric_difference(&now).copied().collect();
        self.dom.checked = Some(now);
        if changed.is_empty() {
            return;
        }
        let track = self.dom.track;
        self.dom.track = true;
        for n in changed {
            if (n as usize) < self.dom.len() && self.dom.is_connected(n) {
                self.dom.note_state(n);
            }
        }
        self.dom.track = track;
    }

    /// The form controls as the DOM is now (recollected if a script changed them).
    pub(crate) fn sync_forms(&mut self) {
        if self.dom.changes.forms {
            self.dom.changes.forms = false;
            self.forms = forms::recollect(&self.dom, &self.forms, 100_000);
            self.attach_form_images();
            self.layout_dirty = true;
        }
    }

    /// The element of control `id`.
    pub(crate) fn control_node(&self, id: ControlId) -> Option<NodeId> {
        self.forms.nodes.get(id.0 as usize).copied()
    }

    /// The control an element is (after [`Page::sync_forms`]).
    pub(crate) fn control_of_node(&self, n: NodeId) -> Option<ControlId> {
        self.forms.by_node.get(&n).map(|c| c.0)
    }

    /// The `<form>` element control `id` belongs to.
    pub(crate) fn form_node_of(&self, id: ControlId) -> Option<NodeId> {
        let f = self.forms.controls.get(id.0 as usize)?.form?;
        self.forms.forms.get(f as usize).copied()
    }

    pub(crate) fn focused_control(&self) -> Option<ControlId> {
        if self.focus == NONE {
            return None;
        }
        self.control_of_node(self.focus)
    }

    pub(crate) fn set_focus_node(&mut self, n: Option<NodeId>) {
        self.focus = n.unwrap_or(NONE);
    }

    pub(crate) fn forms(&self) -> &Forms {
        &self.forms
    }

    pub(crate) fn forms_mut(&mut self) -> &mut Forms {
        &mut self.forms
    }

    /// The element under a viewport point (as [`Page::hit_test`] finds it), NONE if none.
    pub(crate) fn node_at(&self, x: i32, y: i32, scroll: (i32, i32)) -> NodeId {
        let Some(p) = &self.painted else {
            return NONE;
        };
        let (dx, dy) = (x + scroll.0, y + scroll.1);
        for h in p.hits.iter().rev() {
            if h.fixed && h.rect.contains(x, y) {
                return h.node;
            }
        }
        for h in p.hits.iter().rev() {
            if !h.fixed && h.rect.contains(dx, dy) {
                return h.node;
            }
        }
        NONE
    }

    /// A node's box after the last layout (document space; fixed ones in viewport space).
    pub(crate) fn node_rect(&self, n: NodeId) -> Option<Rect> {
        self.painted.as_ref()?.node_rects.get(n as usize).copied()
    }

    pub(crate) fn styles(&self) -> Option<&Styles> {
        self.styles.as_ref()
    }

    pub(crate) fn page_url(&self) -> &str {
        &self.url
    }

    /// The submission of the `<form>` element `form` (with `submitter`, if any).
    pub(crate) fn submit_form_node(
        &self,
        form: NodeId,
        submitter: Option<ControlId>,
    ) -> Option<Submission> {
        let idx = self.forms.forms.iter().position(|&f| f == form)? as u32;
        self.forms
            .submit_form(&self.dom, &self.url, &self.base, idx, submitter, None)
    }

    /// The page's address as script sees it (location.href).
    pub(crate) fn address(&self) -> &str {
        &self.url
    }

    /// history.pushState: the page now has this address (and base, without a `<base>`).
    pub(crate) fn set_address(&mut self, url: &str) {
        if self.base == self.url {
            self.base = String::from(url);
        }
        self.url = String::from(url);
    }

    /// A script changed a control's state: draw again.
    pub(crate) fn mark_forms_changed(&mut self) {
        self.layout_dirty = true;
    }

    pub(crate) fn limits(&self) -> &Limits {
        &self.limits
    }

    /// Does the DOM have changes the display list doesn't show yet?
    pub(crate) fn changed(&self) -> bool {
        let c = &self.dom.changes;
        c.any || c.tree || c.text || c.sheets || c.forms || c.images || self.layout_dirty
    }

    // The is_some + unwrap dodges a borrow-checker limit on returning a borrow early.
    #[allow(clippy::unnecessary_unwrap)]
    pub fn layout(&mut self, vp: &Viewport, m: &dyn TextMeasurer) -> &DisplayList {
        let vp = Viewport {
            zoom: vp.zoom.clamp(25, 500),
            width: vp.width.clamp(1, 1 << 16),
            height: vp.height.clamp(1, 1 << 16),
            dark: vp.dark,
        };
        self.flush();
        if self.last_vp == Some(vp)
            && !self.layout_dirty
            && !self.sheets_changed
            && self.painted.is_some()
        {
            return &self.painted.as_ref().unwrap().dl;
        }
        self.ensure_styled(&vp);
        self.last_vp = Some(vp);
        // The old display list goes before the new one is built (not both at the peak).
        self.painted = None;
        let zoom = vp.zoom as f32 / 100.0;
        let styles = self.styles.as_ref().unwrap();
        let lookup = ImageLookup {
            of_node: &self.img_nodes,
            images: &self.images,
        };
        let inp = layout::Inputs {
            dom: &self.dom,
            styles,
            images: &lookup,
            controls: &self.forms.by_node,
        };
        let cx = Cx::new(&self.dom, m, zoom, vp.width * AU, vp.height * AU);
        let laid = layout::layout(&inp, &cx);
        self.boxes = laid.boxes.len();
        let images = &self.images;
        let size = |id: ImageId| images.get(id.0 as usize).and_then(|i| i.size);
        let by_node = &self.forms.by_node;
        let controls = |n: NodeId| by_node.get(&n).map(|c| c.0);
        let canvas = canvas_color(
            &self.dom,
            styles,
            if self.ua_dark {
                CANVAS_DARK
            } else {
                CANVAS_LIGHT
            },
        );
        let painted = paint::paint(&laid, &cx, &size, &controls, self.dom.len(), canvas);
        // Controls: where they are and how their text should look.
        for c in self.forms.controls.iter_mut() {
            c.rect = Rect::default();
        }
        for &(id, r) in &painted.control_rects {
            if let Some(c) = self.forms.controls.get_mut(id.0 as usize) {
                c.rect = r;
            }
        }
        for (i, &node) in self.forms.nodes.iter().enumerate() {
            if let Some(Some(s)) = styles.of.get(node as usize) {
                self.forms.controls[i].font = cx.font(s);
                self.forms.controls[i].color = s.color;
            }
        }
        drop(cx);
        self.painted = Some(painted);
        self.layout_dirty = false;
        &self.painted.as_ref().unwrap().dl
    }

    pub fn display_list(&self) -> &DisplayList {
        self.painted.as_ref().map(|p| &p.dl).unwrap_or(&self.empty)
    }

    pub fn hit_test(&self, x: i32, y: i32, scroll: (i32, i32)) -> Hit {
        let Some(p) = &self.painted else {
            return Hit::default();
        };
        let (dx, dy) = (x + scroll.0, y + scroll.1);
        let mut found = None;
        for h in p.hits.iter().rev() {
            if h.fixed && h.rect.contains(x, y) {
                found = Some(h);
                break;
            }
        }
        if found.is_none() {
            for h in p.hits.iter().rev() {
                if !h.fixed && h.rect.contains(dx, dy) {
                    found = Some(h);
                    break;
                }
            }
        }
        let Some(h) = found else {
            return Hit::default();
        };
        let mut hit = Hit {
            text: h.text,
            ..Hit::default()
        };
        let mut n = h.node;
        while n != NONE {
            if let Some(e) = self.dom.element(n) {
                if hit.link.is_none()
                    && (e.is(tag::A) || e.is(tag::AREA))
                    && let Some(href) = e.attr("href")
                {
                    hit.link = Some(Link {
                        url: self.resolve(href),
                        target: e.attr("target").map(String::from),
                        download: e.has_attr("download"),
                    });
                }
                if hit.control.is_none() {
                    if let Some(&(id, _)) = self.forms.by_node.get(&n) {
                        hit.control = Some(id);
                    } else if e.is(tag::LABEL) {
                        hit.control = self.label_control(n);
                    }
                }
                if hit.title.is_none()
                    && let Some(t) = e.attr("title")
                    && !t.is_empty()
                {
                    hit.title = Some(String::from(t));
                }
            }
            n = self.dom.parent(n);
        }
        if hit.link.is_some() || hit.control.is_some() {
            hit.text = false;
        }
        hit
    }

    /// The control a `<label>` is for (HTML §4.10.4): `for=id`, else its first labelable
    /// descendant.
    pub(crate) fn label_control(&self, label: NodeId) -> Option<ControlId> {
        let e = self.dom.element(label)?;
        if let Some(f) = e.attr("for") {
            let a = self.dom.atoms.get(f)?;
            for (i, &node) in self.forms.nodes.iter().enumerate() {
                if self.dom.element(node).is_some_and(|x| x.id == Some(a)) {
                    return Some(ControlId(i as u32));
                }
            }
            return None;
        }
        let mut c = self.dom.next_in(label, label);
        while c != NONE {
            if let Some(&(id, _)) = self.forms.by_node.get(&c) {
                return Some(id);
            }
            c = self.dom.next_in(c, label);
        }
        None
    }

    /// The text runs in paint order, each with its owning list (false = scrolling, true =
    /// fixed) and index.
    fn runs(&self) -> Vec<(TextRun, bool)> {
        let Some(p) = &self.painted else {
            return Vec::new();
        };
        let mut v = Vec::new();
        for e in &p.dl.entries {
            if let Item::Text(t) = e.item {
                v.push((t, false));
            }
        }
        for e in &p.dl.fixed {
            if let Item::Text(t) = e.item {
                v.push((t, true));
            }
        }
        v
    }

    pub fn find(&self, query: &str, case_sensitive: bool) -> Vec<FindMatch> {
        let Some(p) = &self.painted else {
            return Vec::new();
        };
        let fold = |c: char| -> char {
            if case_sensitive {
                c
            } else {
                c.to_lowercase().next().unwrap_or(c)
            }
        };
        let q: Vec<char> = query
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .map(fold)
            .collect();
        if q.is_empty() {
            return Vec::new();
        }
        // The page text with each char's run and byte offset (u32::MAX for separators).
        let runs = self.runs();
        let mut hay: Vec<(char, u32, u32)> = Vec::new();
        let mut prev: Option<TextRun> = None;
        for (ri, (r, _)) in runs.iter().enumerate() {
            if let Some(pr) = prev {
                let same_line = pr.baseline == r.baseline && (r.x - (pr.x + pr.width)).abs() <= 1;
                if !same_line {
                    hay.push((' ', u32::MAX, 0));
                }
            }
            let s = p.dl.run_text(r);
            for (off, c) in s.char_indices() {
                hay.push((fold(c), ri as u32, off as u32));
            }
            prev = Some(*r);
            if hay.len() > 8 << 20 {
                break;
            }
        }
        let mut out = Vec::new();
        let n = hay.len();
        let m = q.len();
        let mut i = 0;
        while i + m <= n {
            if hay[i].0 == q[0]
                && (0..m)
                    .all(|k| hay[i + k].0 == q[k] || (q[k] == ' ' && hay[i + k].0.is_whitespace()))
            {
                out.push(FindMatch {
                    rects: self.match_rects(&runs, &hay[i..i + m]),
                });
                i += m;
                if out.len() >= 10_000 {
                    break;
                }
            } else {
                i += 1;
            }
        }
        out
    }

    /// Rectangles of a match: per run, the slice's horizontal extent (estimated from the
    /// approximate advance table, scaled to the run's measured width).
    fn match_rects(&self, runs: &[(TextRun, bool)], chars: &[(char, u32, u32)]) -> Vec<Rect> {
        let Some(p) = &self.painted else {
            return Vec::new();
        };
        let mut rects: Vec<Rect> = Vec::new();
        let am = ApproxMeasurer;
        let mut k = 0;
        while k < chars.len() {
            let ri = chars[k].1;
            if ri == u32::MAX {
                k += 1;
                continue;
            }
            let start = chars[k].2 as usize;
            let mut end_k = k;
            while end_k + 1 < chars.len() && chars[end_k + 1].1 == ri {
                end_k += 1;
            }
            let (r, _) = runs[ri as usize];
            let s = p.dl.run_text(&r);
            let last = chars[end_k].2 as usize;
            let end = last + s[last..].chars().next().map(|c| c.len_utf8()).unwrap_or(0);
            let total = am.text_width(&r.font, s).max(1);
            let a = am.text_width(&r.font, &s[..start]);
            let b = am.text_width(&r.font, &s[..end]);
            let x0 = r.x + (a as i64 * r.width as i64 / total as i64) as i32;
            let x1 = r.x + (b as i64 * r.width as i64 / total as i64) as i32;
            let h = r.font.size as i32;
            rects.push(Rect::new(
                x0,
                r.baseline - h * 4 / 5 - 1,
                (x1 - x0).max(1),
                h + 2,
            ));
            k = end_k + 1;
        }
        rects
    }

    pub fn fragment_y(&self, frag: &str) -> Option<i32> {
        let frag = crate::url::percent_decode(frag);
        if frag.is_empty() || frag.eq_ignore_ascii_case("top") {
            return Some(0);
        }
        let p = self.painted.as_ref()?;
        let a = self.dom.atoms.get(&frag);
        let mut target = NONE;
        let mut n = 0;
        while n != NONE {
            if let Some(e) = self.dom.element(n) {
                if a.is_some() && e.id == a {
                    target = n;
                    break;
                }
                if target == NONE && e.is(tag::A) && e.attr("name") == Some(frag.as_str()) {
                    target = n;
                }
            }
            n = self.dom.next_in(n, 0);
        }
        if target == NONE {
            return None;
        }
        // Its box, or the next thing after it that has one (an empty anchor).
        let mut n = target;
        while n != NONE {
            let r = p.node_rects.get(n as usize).copied().unwrap_or_default();
            if r.w != 0 || r.h != 0 || r.x != 0 || r.y != 0 {
                return Some(r.y.max(0));
            }
            n = self.dom.next_in(n, 0);
        }
        Some(0)
    }

    pub fn main_content(&self) -> Option<Rect> {
        let p = self.painted.as_ref()?;
        let rect = |n: NodeId| {
            p.node_rects
                .get(n as usize)
                .copied()
                .filter(|r| !r.is_empty())
        };
        let mut articles = Vec::new();
        let mut by_id = None;
        let mut n = 0;
        while n != NONE {
            if let Some(e) = self.dom.element(n) {
                if (e.is(tag::MAIN)
                    || e.attr("role")
                        .is_some_and(|r| r.eq_ignore_ascii_case("main")))
                    && let Some(r) = rect(n)
                {
                    return Some(r);
                }
                if e.is(tag::ARTICLE) {
                    articles.push(n);
                }
                if by_id.is_none()
                    && let Some(id) = e.attr("id")
                    && matches!(
                        id,
                        "content" | "main-content" | "main" | "maincontent" | "primary"
                    )
                {
                    by_id = Some(n);
                }
            }
            n = self.dom.next_in(n, 0);
        }
        if articles.len() == 1
            && let Some(r) = rect(articles[0])
        {
            return Some(r);
        }
        by_id.and_then(rect)
    }

    #[cfg(test)]
    pub(crate) fn force_full_restyle(&mut self) {
        self.styles = None;
        self.layout_dirty = true;
    }

    /// Test helper: the border box (union of fragments) of the element with this id.
    #[cfg(test)]
    pub(crate) fn rect_of_id(&self, id: &str) -> Option<Rect> {
        let p = self.painted.as_ref()?;
        let a = self.dom.atoms.get(id)?;
        let mut n = 0;
        while n != NONE {
            if self.dom.element(n).is_some_and(|e| e.id == Some(a)) {
                return p.node_rects.get(n as usize).copied();
            }
            n = self.dom.next_in(n, 0);
        }
        None
    }

    pub fn controls(&self) -> &[FormControl] {
        &self.forms.controls
    }

    pub fn set_value(&mut self, id: ControlId, v: &str) {
        self.forms.set_value(id, v)
    }

    pub fn set_checked(&mut self, id: ControlId, c: bool) {
        self.forms.set_checked(id, c)
    }

    pub fn select_option(&mut self, id: ControlId, i: usize, s: bool) {
        self.forms.select_option(id, i, s)
    }

    pub fn submit(&self, id: ControlId, click: Option<(i32, i32)>) -> Option<Submission> {
        self.forms
            .submit(&self.dom, &self.url, &self.base, id, click)
    }

    pub fn reset(&mut self, id: ControlId) {
        self.forms.reset(id)
    }

    pub fn text(&self) -> String {
        let Some(p) = &self.painted else {
            return String::new();
        };
        let mut s = String::new();
        let mut prev: Option<TextRun> = None;
        for (r, _) in self.runs() {
            if let Some(pr) = prev {
                if pr.baseline != r.baseline {
                    s.push('\n');
                } else if r.x - (pr.x + pr.width) > 1 {
                    s.push(' ');
                }
            }
            s.push_str(p.dl.run_text(&r));
            prev = Some(r);
        }
        s
    }

    pub fn stats(&self) -> Stats {
        let elements = self
            .dom
            .nodes
            .iter()
            .filter(|n| matches!(n.data, NodeData::Element(_)))
            .count();
        let dl_items = self
            .painted
            .as_ref()
            .map(|p| p.dl.entries.len() + p.dl.fixed.len())
            .unwrap_or(0);
        let style_bytes = self
            .styles
            .as_ref()
            .map(|s| {
                s.distinct * core::mem::size_of::<crate::style::types::Style>() + s.of.len() * 8
            })
            .unwrap_or(0);
        let dl_bytes = self
            .painted
            .as_ref()
            .map(|p| {
                p.dl.entries.capacity() * core::mem::size_of::<Entry>()
                    + p.dl.text.capacity()
                    + p.hits.capacity() * 32
                    + p.node_rects.capacity() * 16
            })
            .unwrap_or(0);
        Stats {
            nodes: self.dom.len(),
            elements,
            rules: self.rules.as_ref().map(|r| r.len()).unwrap_or(0),
            boxes: self.boxes,
            display_items: dl_items,
            approx_bytes: self.dom.approx_bytes() + style_bytes + dl_bytes,
            layout_bytes: self.boxes * core::mem::size_of::<crate::layout::LBox>(),
        }
    }
}

fn media_attr(m: Option<&str>) -> MediaList {
    match m {
        Some(m) if !m.trim().is_empty() => sheet::parse_media_list(&tokenize(m, 4096)),
        _ => MediaList::default(),
    }
}

/// The canvas colour (CSS Backgrounds 3 §2.11.2): the root's background, or the body's if
/// the root has none; `default` (milk, or Chocolate milk for a dark page) if neither.
fn canvas_color(dom: &Dom, styles: &Styles, default: Color) -> Color {
    let root = dom.document_element();
    let bg = |n: NodeId| {
        styles
            .of
            .get(n as usize)
            .and_then(|s| s.as_ref())
            .map(|s| s.background_color)
            .unwrap_or(0)
    };
    if root != NONE && bg(root) >> 24 != 0 {
        return blend_over(bg(root), default);
    }
    let body = dom.body();
    if body != NONE && bg(body) >> 24 != 0 {
        return blend_over(bg(body), default);
    }
    default
}

/// `c` (maybe translucent) over the opaque `under`.
fn blend_over(c: Color, under: Color) -> Color {
    let a = c >> 24;
    if a == 255 {
        return c;
    }
    let mix = |s: u32| {
        let (v, u) = ((c >> s) & 255, (under >> s) & 255);
        ((v * a + u * (255 - a)) / 255) << s
    };
    0xFF00_0000 | mix(16) | mix(8) | mix(0)
}
