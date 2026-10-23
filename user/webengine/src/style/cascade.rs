//! The cascade (CSS Cascade 4 §6) and computed values (§4.4): for each element, the matching
//! rules are found through buckets keyed by the rightmost compound's id / class / tag, sorted
//! by origin, importance, specificity and order, custom properties are resolved and `var()`
//! substituted (CSS Variables §3), and the winning declarations are turned into a [`Style`].
//!
//! Order applied (later wins): UA normal, HTML presentational hints, author normal, the
//! `style` attribute, author !important, `style` !important, UA !important.
//!
//! Elements whose parent style and matched rules are the same share one `Rc<Style>` (the
//! style-sharing cache), which keeps memory flat on long, regular pages.

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use super::props::*;
use super::types::*;
use crate::api::{BorderStyle, Decoration, ImageId};
use crate::atoms::{Atom, tag};
use crate::css::selector::{Key, MatchCx, PseudoElement, Selector, key_of};
use crate::css::sheet::{DeclBlock, MediaEnv, Rule, Sheet};
use crate::css::tokenizer::Tok;
use crate::css::values::{self, LenCx, SColor};
use crate::dom::{Dom, Element, NONE, NodeData, NodeId, Ns};

struct Entry {
    sel: Selector,
    decls: Rc<DeclBlock>,
    order: u32,
    ua: bool,
}

/// All rules that apply under one media environment, bucketed for matching.
pub struct RuleSet {
    entries: Vec<Entry>,
    by_id: Vec<Vec<u32>>,
    by_class: Vec<Vec<u32>>,
    by_tag: Vec<Vec<u32>>,
    universal: Vec<u32>,
    before: Vec<u32>,
    after: Vec<u32>,
    pub media_rules: bool,
}

fn bucket(v: &mut Vec<Vec<u32>>, a: Atom, i: u32) {
    let k = a.0 as usize;
    if v.len() <= k {
        v.resize_with(k + 1, Vec::new);
    }
    v[k].push(i);
}

impl RuleSet {
    /// Flatten `sheets` (each with whether it is the UA sheet) under `env`. `imports` maps an
    /// @import URL to its parsed sheet.
    pub fn build(
        sheets: &[(Rc<Sheet>, bool)],
        imports: &BTreeMap<String, Rc<Sheet>>,
        env: &MediaEnv,
    ) -> RuleSet {
        let mut rs = RuleSet {
            entries: Vec::new(),
            by_id: Vec::new(),
            by_class: Vec::new(),
            by_tag: Vec::new(),
            universal: Vec::new(),
            before: Vec::new(),
            after: Vec::new(),
            media_rules: false,
        };
        for (s, ua) in sheets {
            rs.add_rules(&s.rules, *ua, imports, env, 0);
        }
        rs
    }

    fn add_rules(
        &mut self,
        rules: &[Rule],
        ua: bool,
        imports: &BTreeMap<String, Rc<Sheet>>,
        env: &MediaEnv,
        depth: u32,
    ) {
        if depth > 8 {
            return;
        }
        for r in rules {
            match r {
                Rule::Style { selectors, decls } => {
                    for sel in selectors {
                        let i = self.entries.len() as u32;
                        match sel.pseudo {
                            Some(PseudoElement::Before) => self.before.push(i),
                            Some(PseudoElement::After) => self.after.push(i),
                            Some(PseudoElement::Unsupported) => continue,
                            None => match key_of(sel) {
                                Key::Id(a) => bucket(&mut self.by_id, a, i),
                                Key::Class(a) => bucket(&mut self.by_class, a, i),
                                Key::Tag(a) => bucket(&mut self.by_tag, a, i),
                                Key::Universal => self.universal.push(i),
                            },
                        }
                        self.entries.push(Entry {
                            sel: sel.clone(),
                            decls: decls.clone(),
                            order: i,
                            ua,
                        });
                    }
                }
                Rule::Media { query, rules } => {
                    self.media_rules = true;
                    if query.matches(env) {
                        self.add_rules(rules, ua, imports, env, depth + 1);
                    }
                }
                Rule::Import { url, media } => {
                    if !media.is_empty() {
                        self.media_rules = true;
                    }
                    if media.matches(env)
                        && let Some(s) = imports.get(url)
                    {
                        let s = s.clone();
                        self.add_rules(&s.rules, ua, imports, env, depth + 1);
                    }
                }
            }
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The entries matching `el`, sorted into cascade order (UA first, then by specificity and
    /// order).
    fn matching(&self, cx: &MatchCx, el: NodeId, e: &Element, out: &mut Vec<u32>) {
        out.clear();
        let try_list = |list: &[u32], out: &mut Vec<u32>| {
            for &i in list {
                if cx.matches(&self.entries[i as usize].sel, el) {
                    out.push(i);
                }
            }
        };
        if let Some(id) = e.id
            && let Some(l) = self.by_id.get(id.0 as usize)
        {
            try_list(l, out);
        }
        for c in &e.classes {
            if let Some(l) = self.by_class.get(c.0 as usize) {
                try_list(l, out);
            }
        }
        if let Some(l) = self.by_tag.get(e.name.0 as usize) {
            try_list(l, out);
        }
        try_list(&self.universal, out);
        self.sort(out);
    }

    fn sort(&self, out: &mut Vec<u32>) {
        out.sort_unstable_by_key(|&i| {
            let e = &self.entries[i as usize];
            (!e.ua, e.sel.specificity, e.order)
        });
        out.dedup();
    }

    fn matching_pseudo(&self, cx: &MatchCx, el: NodeId, after: bool, out: &mut Vec<u32>) {
        out.clear();
        for &i in if after { &self.after } else { &self.before } {
            if cx.matches(&self.entries[i as usize].sel, el) {
                out.push(i);
            }
        }
        self.sort(out);
    }
}

/// The computed styles of a document.
pub struct Styles {
    /// Per node (elements only; None for display: none subtrees and non-elements).
    pub of: Vec<Option<Rc<Style>>>,
    pub before: BTreeMap<NodeId, Rc<Style>>,
    pub after: BTreeMap<NodeId, Rc<Style>>,
    pub root_font_size: f32,
    pub distinct: usize,
}

/// Inputs to styling besides the DOM and rules.
pub struct StyleCx<'a> {
    pub env: MediaEnv,
    pub resolve: &'a dyn Fn(&str) -> String,
    pub image: &'a mut dyn FnMut(&str) -> ImageId,
}

struct Styler<'a, 'b> {
    dom: &'a Dom,
    rules: &'a RuleSet,
    cx: &'a mut StyleCx<'b>,
    init: Rc<Style>,
    root_fs: f32,
    index: Vec<u32>,
    count: Vec<u32>,
    cache: BTreeMap<(usize, Vec<u32>), Rc<Style>>,
    slots: Vec<Option<Decl>>,
    scratch: Vec<u32>,
    distinct: usize,
}

/// Style every element of `dom`.
pub fn style_document(dom: &Dom, rules: &RuleSet, cx: &mut StyleCx) -> Styles {
    // Sibling positions for :nth-child() and friends.
    let n = dom.len();
    let mut index = alloc::vec![0u32; n];
    let mut count = alloc::vec![0u32; n];
    for p in 0..n as u32 {
        let mut k = 0;
        let mut c = dom.node(p).first_child;
        while c != NONE {
            if dom.element(c).is_some() {
                k += 1;
                index[c as usize] = k;
            }
            c = dom.node(c).next;
        }
        let mut c = dom.node(p).first_child;
        while c != NONE {
            if dom.element(c).is_some() {
                count[c as usize] = k;
            }
            c = dom.node(c).next;
        }
    }
    let mut st = Styler {
        dom,
        rules,
        cx,
        init: Rc::new(Style::initial()),
        root_fs: 16.0,
        index,
        count,
        cache: BTreeMap::new(),
        slots: alloc::vec![None; PROP_COUNT],
        scratch: Vec::new(),
        distinct: 0,
    };
    let mut out = Styles {
        of: alloc::vec![None; n],
        before: BTreeMap::new(),
        after: BTreeMap::new(),
        root_font_size: 16.0,
        distinct: 0,
    };
    let root = dom.document_element();
    if root == NONE {
        return out;
    }
    // Iterative pre-order walk with the parent style at hand.
    let doc_style = st.init.clone();
    let mut stack: Vec<(NodeId, Rc<Style>)> = alloc::vec![(root, doc_style)];
    while let Some((el, parent)) = stack.pop() {
        let Some(e) = dom.element(el) else { continue };
        let is_root = el == root;
        let s = st.style_element(el, e, &parent, is_root);
        if is_root {
            st.root_fs = s.font_size;
            out.root_font_size = s.font_size;
        }
        if s.display == Display::None {
            continue;
        }
        for (after, map) in [(false, &mut out.before), (true, &mut out.after)] {
            if let Some(ps) = st.style_pseudo(el, &s, after) {
                map.insert(el, ps);
            }
        }
        // Children in reverse so they pop in order.
        let kids: Vec<NodeId> = dom
            .children(el)
            .filter(|&c| dom.element(c).is_some())
            .collect();
        for &c in kids.iter().rev() {
            stack.push((c, s.clone()));
        }
        out.of[el as usize] = Some(s);
    }
    out.distinct = st.distinct;
    out
}

impl Styler<'_, '_> {
    fn style_element(
        &mut self,
        el: NodeId,
        e: &Element,
        parent: &Rc<Style>,
        is_root: bool,
    ) -> Rc<Style> {
        let mcx = MatchCx {
            dom: self.dom,
            index: &self.index,
            count: &self.count,
        };
        let mut matched = core::mem::take(&mut self.scratch);
        self.rules.matching(&mcx, el, e, &mut matched);
        let mut hints = Vec::new();
        presentational_hints(self.dom, el, e, &mut hints);
        let inline = e.attr("style").map(|s| {
            let r = self.cx.resolve;
            crate::css::sheet::parse_inline(s, &ParseCx { resolve: r })
        });
        let shareable = hints.is_empty() && inline.is_none() && !is_root;
        let key = (Rc::as_ptr(parent) as usize, matched.clone());
        if shareable && let Some(s) = self.cache.get(&key) {
            self.scratch = matched;
            return s.clone();
        }
        let s = Rc::new(self.cascade(&matched, &hints, inline.as_ref(), parent, is_root, Some(e)));
        self.distinct += 1;
        if shareable && self.cache.len() < 50_000 {
            self.cache.insert(key, s.clone());
        }
        self.scratch = matched;
        s
    }

    fn style_pseudo(&mut self, el: NodeId, parent: &Rc<Style>, after: bool) -> Option<Rc<Style>> {
        let list = if after {
            &self.rules.after
        } else {
            &self.rules.before
        };
        if list.is_empty() {
            return None;
        }
        let mcx = MatchCx {
            dom: self.dom,
            index: &self.index,
            count: &self.count,
        };
        let mut matched = Vec::new();
        self.rules.matching_pseudo(&mcx, el, after, &mut matched);
        if matched.is_empty() {
            return None;
        }
        let s = self.cascade(&matched, &[], None, parent, false, None);
        let has = s.content.as_ref().is_some_and(|c| !c.is_empty()) && s.display != Display::None;
        if !has {
            return None;
        }
        Some(Rc::new(s))
    }

    /// Run the cascade for one element and compute its style.
    fn cascade(
        &mut self,
        matched: &[u32],
        hints: &[Decl],
        inline: Option<&DeclBlock>,
        parent: &Style,
        is_root: bool,
        el: Option<&Element>,
    ) -> Style {
        let rules = self.rules;
        let ua_n = matched
            .iter()
            .take_while(|&&i| rules.entries[i as usize].ua)
            .count();
        let (ua, author) = matched.split_at(ua_n);
        // The declaration groups in increasing precedence.
        let mut groups: Vec<&[Decl]> = Vec::new();
        for &i in ua {
            groups.push(&rules.entries[i as usize].decls.normal);
        }
        groups.push(hints);
        for &i in author {
            groups.push(&rules.entries[i as usize].decls.normal);
        }
        if let Some(b) = inline {
            groups.push(&b.normal);
        }
        for &i in author {
            groups.push(&rules.entries[i as usize].decls.important);
        }
        if let Some(b) = inline {
            groups.push(&b.important);
        }
        for &i in ua {
            groups.push(&rules.entries[i as usize].decls.important);
        }

        // Pass 1: custom properties (inherited; shared with the parent unless changed).
        let mut custom = parent.custom.clone();
        let mut changed = false;
        for g in &groups {
            for d in g.iter() {
                if let Decl::Custom(n, v) = d {
                    let m = Rc::make_mut(custom.get_or_insert_with(|| Rc::new(CustomMap::new())));
                    m.insert(n.clone(), v.clone());
                    changed = true;
                }
            }
        }
        if changed && let Some(map) = custom.as_mut() {
            // Resolve var() inside custom values (bounded depth; cycles become invalid).
            let keys: Vec<Rc<str>> = map
                .iter()
                .filter(|(_, v)| {
                    v.iter()
                        .any(|t| matches!(t, Tok::Function(f) if f == "var"))
                })
                .map(|(k, _)| k.clone())
                .collect();
            if !keys.is_empty() {
                let snapshot = map.clone();
                let m = Rc::make_mut(map);
                for k in keys {
                    let v = snapshot.get(&k).cloned().unwrap();
                    match substitute(&v, &snapshot, 0) {
                        Some(t) => {
                            m.insert(k, Rc::from(t));
                        }
                        None => {
                            m.remove(&k);
                        }
                    }
                }
            }
        }

        // Pass 2: the winning declaration per longhand.
        for s in self.slots.iter_mut() {
            *s = None;
        }
        let resolve = self.cx.resolve;
        let pcx = ParseCx { resolve };
        for g in &groups {
            for d in g.iter() {
                match d {
                    Decl::Custom(..) => {}
                    Decl::Pending(name, toks) => {
                        let empty = CustomMap::new();
                        let map = custom.as_deref().unwrap_or(&empty);
                        if let Some(t) = substitute(toks, map, 0) {
                            let mut tmp = Vec::new();
                            parse_declaration(name, &t, &pcx, &mut tmp);
                            for x in tmp {
                                if let Some(p) = x.prop() {
                                    self.slots[p as usize] = Some(x);
                                }
                            }
                        }
                    }
                    d => {
                        if let Some(p) = d.prop() {
                            self.slots[p as usize] = Some(d.clone());
                        }
                    }
                }
            }
        }

        // Compute.
        let mut s = Style::inherit(parent, &self.init);
        s.custom = custom;
        let lcx_parent = LenCx {
            em: parent.font_size,
            rem: self.root_fs,
            vw: self.cx.env.width,
            vh: self.cx.env.height,
        };
        // font-size first (em units of everything else depend on it), then color (currentColor).
        if let Some(d) = self.slots[Prop::FontSize as usize].take() {
            self.apply(&mut s, parent, &d, &lcx_parent, is_root);
        }
        if is_root {
            self.root_fs = s.font_size;
        }
        let lcx = LenCx {
            em: s.font_size,
            rem: if is_root { s.font_size } else { self.root_fs },
            vw: self.cx.env.width,
            vh: self.cx.env.height,
        };
        if let Some(d) = self.slots[Prop::Color as usize].take() {
            self.apply(&mut s, parent, &d, &lcx, is_root);
        }
        let mut border_colors: [Option<SColor>; 4] = [Some(SColor::CurrentColor); 4];
        let mut deco_color = None;
        for i in 0..PROP_COUNT {
            let Some(d) = self.slots[i].take() else {
                continue;
            };
            match &d {
                Decl::BorderColor(side, c) => border_colors[*side as usize] = Some(*c),
                Decl::TextDecorationColor(c) => deco_color = Some(*c),
                Decl::Wide(p, w) if matches!(*p as u8, x if x >= Prop::BorderColor0 as u8 && x <= Prop::BorderColor3 as u8) =>
                {
                    let side = *p as u8 - Prop::BorderColor0 as u8;
                    border_colors[side as usize] = None;
                    self.apply(&mut s, parent, &Decl::Wide(*p, *w), &lcx, is_root);
                }
                _ => self.apply(&mut s, parent, &d, &lcx, is_root),
            }
        }
        for (i, c) in border_colors.iter().enumerate() {
            if let Some(c) = c {
                s.border_color[i] = resolve_color(*c, s.color);
            }
        }
        s.deco_color = deco_color.map(|c| resolve_color(c, s.color));
        // Border widths are 0 for none/hidden (CSS Backgrounds 3 §4.3).
        for i in 0..4 {
            if s.border_style[i] == BorderStyle::None {
                s.border_width[i] = 0.0;
            }
        }
        // Display fixups (CSS 2.1 §9.7, CSS Display §2.7, Flexbox §4).
        s.original_display = s.display;
        if s.is_out_of_flow_positioned() {
            s.float = Float::None;
        }
        let parent_flex =
            matches!(parent.display, Display::Flex | Display::InlineFlex) && el.is_some();
        if is_root || s.float != Float::None || s.is_out_of_flow_positioned() || parent_flex {
            if s.display != Display::None && s.display != Display::Contents {
                s.display = s.display.blockify();
            }
            if parent_flex && s.display == Display::ListItem {
                // stays a list-item (blockified already)
            }
        }
        // Text decorations propagate to in-flow descendants, but not into atomic inlines,
        // floats or absolutely positioned boxes (CSS Text Decoration 3 §2.1).
        let breaks = s.original_display.is_inline_level() && s.original_display != Display::Inline
            || s.float != Float::None
            || s.is_out_of_flow_positioned();
        if breaks {
            s.deco_in_effect = Decoration::NONE;
        }
        if s.deco_line.0 != 0 {
            s.deco_in_effect = Decoration(s.deco_in_effect.0 | s.deco_line.0);
            s.deco_color_in_effect = s.deco_color.unwrap_or(s.color);
        }
        s
    }

    fn apply(&mut self, s: &mut Style, parent: &Style, d: &Decl, c: &LenCx, is_root: bool) {
        let lp = |l: &values::Len| l.compute(c);
        let lpa = |l: &SLPA| match l {
            SLPA::Auto => LPA::Auto,
            SLPA::L(l) => LPA::L(l.compute(c)),
        };
        let size = |l: &SSize| match l {
            SSize::Auto => Size::Auto,
            SSize::L(l) => Size::L(l.compute(c)),
            SSize::MinContent => Size::MinContent,
            SSize::MaxContent => Size::MaxContent,
            SSize::FitContent => Size::FitContent,
            SSize::Content => Size::Content,
        };
        let px = |l: &values::Len| l.compute(c).px;
        match d {
            Decl::Display(v) => s.display = *v,
            Decl::Position(v) => s.position = *v,
            Decl::Float(v) => s.float = *v,
            Decl::Clear(v) => s.clear = *v,
            Decl::Inset(i, v) => s.inset[*i as usize & 3] = lpa(v),
            Decl::Width(v) => s.width = size(v),
            Decl::Height(v) => s.height = size(v),
            Decl::MinWidth(v) => s.min_width = size(v),
            Decl::MinHeight(v) => s.min_height = size(v),
            Decl::MaxWidth(v) => s.max_width = v.as_ref().map(lp),
            Decl::MaxHeight(v) => s.max_height = v.as_ref().map(lp),
            Decl::Margin(i, v) => s.margin[*i as usize & 3] = lpa(v),
            Decl::Padding(i, v) => s.padding[*i as usize & 3] = lp(v),
            Decl::BorderWidth(i, v) => s.border_width[*i as usize & 3] = px(v).max(0.0),
            Decl::BorderStyle(i, v) => s.border_style[*i as usize & 3] = *v,
            Decl::BorderColor(i, v) => s.border_color[*i as usize & 3] = resolve_color(*v, s.color),
            Decl::BorderRadius(v) => s.border_radius = lp(v),
            Decl::BoxSizing(v) => s.box_sizing = *v,
            Decl::Color(v) => s.color = resolve_color(*v, parent.color),
            Decl::BackgroundColor(v) => s.background_color = resolve_color(*v, s.color),
            Decl::BackgroundImage(v) => s.bg_image = v.as_ref().map(|u| (self.cx.image)(u)),
            Decl::BackgroundRepeat(v) => s.bg_repeat = *v,
            Decl::BackgroundPosition(x, y) => s.bg_position = (lp(x), lp(y)),
            Decl::BackgroundSize(v) => {
                s.bg_size = match v {
                    SBgSize::Auto => BgSize::Auto,
                    SBgSize::Cover => BgSize::Cover,
                    SBgSize::Contain => BgSize::Contain,
                    SBgSize::L(a, b) => BgSize::L(lpa(a), lpa(b)),
                }
            }
            Decl::FontFamily(v) => s.family = *v,
            Decl::FontSize(v) => {
                let p = parent.font_size;
                s.font_size = match v {
                    // em and % against the parent's size (c is the parent's context here).
                    SFontSize::L(l) => l.compute(c).resolve(p),
                    SFontSize::Smaller => p / 1.2,
                    SFontSize::Larger => p * 1.2,
                }
                .clamp(0.0, 10_000.0);
                if is_root {
                    self.root_fs = s.font_size;
                }
            }
            Decl::FontWeight(v) => {
                s.font_weight = match v {
                    SWeight::Abs(w) => *w,
                    // CSS Fonts 4 §2.2 "bolder"/"lighter" table.
                    SWeight::Bolder => match parent.font_weight {
                        0..=349 => 400,
                        350..=549 => 700,
                        _ => 900,
                    },
                    SWeight::Lighter => match parent.font_weight {
                        0..=549 => 100,
                        550..=749 => 400,
                        _ => 700,
                    },
                }
            }
            Decl::FontStyle(v) => s.italic = *v,
            Decl::FontVariant(v) => s.small_caps = *v,
            Decl::LineHeight(v) => {
                s.line_height = match v {
                    SLineHeight::Normal => LineHeight::Normal,
                    SLineHeight::Number(n) => LineHeight::Number(*n),
                    SLineHeight::L(l) => LineHeight::Px(l.compute(c).resolve(s.font_size)),
                }
            }
            Decl::TextAlign(v) => {
                s.text_align = match v {
                    STextAlign::Left | STextAlign::Start => TextAlign::Left,
                    STextAlign::Right | STextAlign::End => TextAlign::Right,
                    STextAlign::Center => TextAlign::Center,
                    STextAlign::Justify => TextAlign::Justify,
                    STextAlign::MatchParent => parent.text_align,
                }
            }
            Decl::TextIndent(v) => s.text_indent = lp(v),
            Decl::TextTransform(v) => s.text_transform = *v,
            Decl::WhiteSpace(v) => s.white_space = *v,
            Decl::TextDecorationLine(v) => s.deco_line = *v,
            Decl::TextDecorationColor(v) => s.deco_color = Some(resolve_color(*v, s.color)),
            Decl::VerticalAlign(v) => {
                s.vertical_align = match v {
                    SVAlign::K(k) => *k,
                    SVAlign::L(l) => {
                        let x = l.compute(c);
                        // % of the line height.
                        let lh = s.line_height_px(s.font_size * 1.2);
                        VAlign::L(LP::px(x.resolve(lh)))
                    }
                }
            }
            Decl::Visibility(v) => s.visibility = *v,
            Decl::OverflowX(v) => s.overflow_x = *v,
            Decl::OverflowY(v) => s.overflow_y = *v,
            Decl::ListStyleType(t, st) => {
                s.list_style_type = *t;
                s.list_style_string = st.clone();
            }
            Decl::ListStylePosition(v) => s.list_inside = *v,
            Decl::BorderCollapse(v) => s.border_collapse = *v,
            Decl::BorderSpacing(h, v) => s.border_spacing = (px(h).max(0.0), px(v).max(0.0)),
            Decl::CaptionSide(v) => s.caption_bottom = *v,
            Decl::TableLayout(v) => s.table_fixed = *v,
            Decl::Cursor(v) => s.cursor = *v,
            Decl::BreakAnywhere(v) => s.break_anywhere = *v,
            Decl::LetterSpacing(v) => s.letter_spacing = v.as_ref().map(px).unwrap_or(0.0),
            Decl::WordSpacing(v) => s.word_spacing = v.as_ref().map(px).unwrap_or(0.0),
            Decl::ZIndex(v) => s.z_index = *v,
            Decl::Opacity(v) => s.opacity = *v,
            Decl::Content(v) => s.content = v.clone(),
            Decl::FlexDirection(v) => s.flex_direction = *v,
            Decl::FlexWrap(v) => s.flex_wrap = *v,
            Decl::JustifyContent(v) => s.justify_content = *v,
            Decl::AlignItems(v) => s.align_items = *v,
            Decl::AlignSelf(v) => s.align_self = *v,
            Decl::AlignContent(v) => s.align_content = *v,
            Decl::FlexGrow(v) => s.flex_grow = *v,
            Decl::FlexShrink(v) => s.flex_shrink = *v,
            Decl::FlexBasis(v) => s.flex_basis = size(v),
            Decl::Order(v) => s.order = *v,
            Decl::RowGap(v) => s.row_gap = lp(v),
            Decl::ColumnGap(v) => s.column_gap = lp(v),
            Decl::AspectRatio(v) => s.aspect_ratio = *v,
            Decl::CounterReset(v) => s.counter_reset = v.clone(),
            Decl::CounterIncrement(v) => s.counter_increment = v.clone(),
            Decl::Wide(p, w) => {
                let inherited = is_inherited(*p);
                let from_parent = match w {
                    Wide::Inherit => true,
                    Wide::Initial => false,
                    Wide::Unset => inherited,
                };
                let init = self.init.clone();
                copy_prop(s, if from_parent { parent } else { &init }, *p);
            }
            Decl::Pending(..) | Decl::Custom(..) => {}
        }
    }
}

fn resolve_color(c: SColor, current: u32) -> u32 {
    match c {
        SColor::Rgba(v) => v,
        SColor::CurrentColor => current,
    }
}

fn is_inherited(p: Prop) -> bool {
    use Prop::*;
    matches!(
        p,
        Color
            | FontFamily
            | FontSize
            | FontWeight
            | FontStyle
            | FontVariant
            | LineHeight
            | TextAlign
            | TextIndent
            | TextTransform
            | WhiteSpace
            | Visibility
            | ListStyleType
            | ListStylePosition
            | BorderCollapse
            | BorderSpacing
            | CaptionSide
            | Cursor
            | BreakAnywhere
            | LetterSpacing
            | WordSpacing
    )
}

/// Copy one longhand's computed value (for inherit / initial / unset).
fn copy_prop(s: &mut Style, from: &Style, p: Prop) {
    use Prop::*;
    let side = |base: Prop| (p as u8 - base as u8) as usize;
    match p {
        Display => s.display = from.display,
        Position => s.position = from.position,
        Float => s.float = from.float,
        Clear => s.clear = from.clear,
        Inset0 | Inset1 | Inset2 | Inset3 => s.inset[side(Inset0)] = from.inset[side(Inset0)],
        Width => s.width = from.width,
        Height => s.height = from.height,
        MinWidth => s.min_width = from.min_width,
        MinHeight => s.min_height = from.min_height,
        MaxWidth => s.max_width = from.max_width,
        MaxHeight => s.max_height = from.max_height,
        Margin0 | Margin1 | Margin2 | Margin3 => {
            s.margin[side(Margin0)] = from.margin[side(Margin0)]
        }
        Padding0 | Padding1 | Padding2 | Padding3 => {
            s.padding[side(Padding0)] = from.padding[side(Padding0)]
        }
        BorderWidth0 | BorderWidth1 | BorderWidth2 | BorderWidth3 => {
            s.border_width[side(BorderWidth0)] = from.border_width[side(BorderWidth0)]
        }
        BorderStyle0 | BorderStyle1 | BorderStyle2 | BorderStyle3 => {
            s.border_style[side(BorderStyle0)] = from.border_style[side(BorderStyle0)]
        }
        BorderColor0 | BorderColor1 | BorderColor2 | BorderColor3 => {
            s.border_color[side(BorderColor0)] = from.border_color[side(BorderColor0)]
        }
        BorderRadius => s.border_radius = from.border_radius,
        BoxSizing => s.box_sizing = from.box_sizing,
        Color => s.color = from.color,
        BackgroundColor => s.background_color = from.background_color,
        BackgroundImage => s.bg_image = from.bg_image,
        BackgroundRepeat => s.bg_repeat = from.bg_repeat,
        BackgroundPosition => s.bg_position = from.bg_position,
        BackgroundSize => s.bg_size = from.bg_size,
        FontFamily => s.family = from.family,
        FontSize => s.font_size = from.font_size,
        FontWeight => s.font_weight = from.font_weight,
        FontStyle => s.italic = from.italic,
        FontVariant => s.small_caps = from.small_caps,
        LineHeight => s.line_height = from.line_height,
        TextAlign => s.text_align = from.text_align,
        TextIndent => s.text_indent = from.text_indent,
        TextTransform => s.text_transform = from.text_transform,
        WhiteSpace => s.white_space = from.white_space,
        TextDecorationLine => s.deco_line = from.deco_line,
        TextDecorationColor => s.deco_color = from.deco_color,
        VerticalAlign => s.vertical_align = from.vertical_align,
        Visibility => s.visibility = from.visibility,
        OverflowX => s.overflow_x = from.overflow_x,
        OverflowY => s.overflow_y = from.overflow_y,
        ListStyleType => {
            s.list_style_type = from.list_style_type;
            s.list_style_string = from.list_style_string.clone();
        }
        ListStylePosition => s.list_inside = from.list_inside,
        BorderCollapse => s.border_collapse = from.border_collapse,
        BorderSpacing => s.border_spacing = from.border_spacing,
        CaptionSide => s.caption_bottom = from.caption_bottom,
        TableLayout => s.table_fixed = from.table_fixed,
        Cursor => s.cursor = from.cursor,
        BreakAnywhere => s.break_anywhere = from.break_anywhere,
        LetterSpacing => s.letter_spacing = from.letter_spacing,
        WordSpacing => s.word_spacing = from.word_spacing,
        ZIndex => s.z_index = from.z_index,
        Opacity => s.opacity = from.opacity,
        Content => s.content = from.content.clone(),
        FlexDirection => s.flex_direction = from.flex_direction,
        FlexWrap => s.flex_wrap = from.flex_wrap,
        JustifyContent => s.justify_content = from.justify_content,
        AlignItems => s.align_items = from.align_items,
        AlignSelf => s.align_self = from.align_self,
        AlignContent => s.align_content = from.align_content,
        FlexGrow => s.flex_grow = from.flex_grow,
        FlexShrink => s.flex_shrink = from.flex_shrink,
        FlexBasis => s.flex_basis = from.flex_basis,
        Order => s.order = from.order,
        RowGap => s.row_gap = from.row_gap,
        ColumnGap => s.column_gap = from.column_gap,
        AspectRatio => s.aspect_ratio = from.aspect_ratio,
        CounterReset => s.counter_reset = from.counter_reset.clone(),
        CounterIncrement => s.counter_increment = from.counter_increment.clone(),
        Count => {}
    }
}

/// CSS Variables §3.1 "substitute a var()": replace each `var(--x[, fallback])`. None if a
/// reference can't be resolved (the declaration is then invalid at computed-value time).
fn substitute(t: &[Tok], map: &CustomMap, depth: u32) -> Option<Vec<Tok>> {
    if depth > 8 {
        return None;
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < t.len() {
        match &t[i] {
            Tok::Function(f) if f == "var" || f == "env" => {
                let end = values::component_end(t, i);
                let args = values::fn_args(&t[i..end]);
                let a = values::skip_ws(args, 0);
                let name = match args.get(a) {
                    Some(Tok::Ident(n)) => n.as_str(),
                    _ => return None,
                };
                let comma = args.iter().position(|x| matches!(x, Tok::Comma));
                let val = if f == "env" { None } else { map.get(name) };
                match val {
                    Some(v) => {
                        let inner = substitute(v, map, depth + 1)?;
                        out.extend(inner);
                    }
                    None => {
                        let c = comma?;
                        let fb = values::trim(&args[c + 1..]);
                        out.extend(substitute(fb, map, depth + 1)?);
                    }
                }
                if out.len() > 10_000 {
                    return None;
                }
                i = end;
            }
            x => {
                out.push(x.clone());
                i += 1;
            }
        }
    }
    Some(out)
}

// ---- presentational hints (HTML §15.3 / §15.4) ---------------------------------------------

/// A dimension attribute (HTML §2.3.4.4 "rules for parsing dimension values"): pixels or a
/// percentage.
fn dimension(v: &str) -> Option<values::Len> {
    let v = v.trim_start();
    let digits: String = v
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    if digits.is_empty() {
        return None;
    }
    let n: f32 = crate::css::tokenizer::parse_f32(&digits);
    let rest = &v[digits.len()..];
    if rest.starts_with('%') {
        Some(values::Len::Lin(values::SLen::pct(n)))
    } else {
        Some(values::Len::px(n))
    }
}

fn color_hint(v: &str) -> Option<SColor> {
    values::legacy_color(v).map(SColor::Rgba)
}

fn align_hint(v: &str, out: &mut Vec<Decl>) {
    let a = match v.trim().to_ascii_lowercase().as_str() {
        "left" => STextAlign::Left,
        "right" => STextAlign::Right,
        "center" | "middle" => STextAlign::Center,
        "justify" => STextAlign::Justify,
        _ => return,
    };
    out.push(Decl::TextAlign(a));
}

fn valign_hint(v: &str, out: &mut Vec<Decl>) {
    let a = match v.trim().to_ascii_lowercase().as_str() {
        "top" => VAlign::Top,
        "middle" | "center" => VAlign::Middle,
        "bottom" => VAlign::Bottom,
        "baseline" => VAlign::Baseline,
        _ => return,
    };
    out.push(Decl::VerticalAlign(SVAlign::K(a)));
}

fn nearest_table<'a>(dom: &'a Dom, mut n: NodeId) -> Option<&'a Element> {
    for _ in 0..4 {
        n = dom.parent(n);
        if n == NONE {
            return None;
        }
        if let Some(e) = dom.element(n)
            && e.is(tag::TABLE)
        {
            return Some(e);
        }
    }
    None
}

pub fn presentational_hints(dom: &Dom, el: NodeId, e: &Element, out: &mut Vec<Decl>) {
    use tag::*;
    if e.ns != Ns::Html {
        // Foreign content (SVG/MathML) is not rendered: no box.
        if dom
            .element(dom.parent(el))
            .is_some_and(|p| p.ns == Ns::Html)
        {
            out.push(Decl::Display(Display::None));
        }
        return;
    }
    let attr = |n: &str| e.attr(n);
    let bg = |out: &mut Vec<Decl>| {
        if let Some(c) = attr("bgcolor").and_then(color_hint) {
            out.push(Decl::BackgroundColor(c));
        }
    };
    let wh = |out: &mut Vec<Decl>| {
        if let Some(w) = attr("width").and_then(dimension) {
            out.push(Decl::Width(SSize::L(w)));
        }
        if let Some(h) = attr("height").and_then(dimension) {
            out.push(Decl::Height(SSize::L(h)));
        }
    };
    match e.name {
        BODY => {
            bg(out);
            if let Some(c) = attr("text").and_then(color_hint) {
                out.push(Decl::Color(c));
            }
            for (a, side) in [
                ("marginwidth", [1u8, 3]),
                ("leftmargin", [3, 3]),
                ("marginheight", [0, 2]),
                ("topmargin", [0, 0]),
            ] {
                if let Some(d) = attr(a).and_then(dimension) {
                    for s in side {
                        out.push(Decl::Margin(s, SLPA::L(d.clone())));
                    }
                }
            }
        }
        TABLE => {
            bg(out);
            wh(out);
            if let Some(b) = attr("border") {
                let w = b.trim().parse::<f32>().unwrap_or(1.0).clamp(0.0, 100.0);
                if w > 0.0 {
                    for s in 0..4 {
                        out.push(Decl::BorderWidth(s, values::Len::px(w)));
                        out.push(Decl::BorderStyle(s, BorderStyle::Shaded));
                    }
                }
            }
            if let Some(cs) = attr("cellspacing").and_then(dimension) {
                out.push(Decl::BorderSpacing(cs.clone(), cs));
            }
            match attr("align")
                .map(|a| a.trim().to_ascii_lowercase())
                .as_deref()
            {
                Some("left") => out.push(Decl::Float(Float::Left)),
                Some("right") => out.push(Decl::Float(Float::Right)),
                Some("center") => {
                    out.push(Decl::Margin(1, SLPA::Auto));
                    out.push(Decl::Margin(3, SLPA::Auto));
                }
                _ => {}
            }
        }
        TD | TH => {
            bg(out);
            wh(out);
            if let Some(a) = attr("align") {
                align_hint(a, out);
            }
            if let Some(v) = attr("valign") {
                valign_hint(v, out);
            }
            if e.has_attr("nowrap") {
                out.push(Decl::WhiteSpace(WhiteSpace::Nowrap));
            }
            if let Some(t) = nearest_table(dom, el) {
                if let Some(p) = t.attr("cellpadding").and_then(dimension) {
                    for s in 0..4 {
                        out.push(Decl::Padding(s, p.clone()));
                    }
                }
                if let Some(b) = t.attr("border")
                    && b.trim().parse::<f32>().unwrap_or(1.0) > 0.0
                {
                    for s in 0..4 {
                        out.push(Decl::BorderWidth(s, values::Len::px(1.0)));
                        out.push(Decl::BorderStyle(s, BorderStyle::Shaded));
                    }
                }
            }
        }
        TR | THEAD | TBODY | TFOOT => {
            bg(out);
            if let Some(a) = attr("align") {
                align_hint(a, out);
            }
            if let Some(v) = attr("valign") {
                valign_hint(v, out);
            }
            if e.name == TR
                && let Some(h) = attr("height").and_then(dimension)
            {
                out.push(Decl::Height(SSize::L(h)));
            }
        }
        COL | COLGROUP => {
            if let Some(w) = attr("width").and_then(dimension) {
                out.push(Decl::Width(SSize::L(w)));
            }
        }
        IMG | IFRAME | EMBED | OBJECT | VIDEO | CANVAS => {
            wh(out);
            if let Some(b) = attr("border").and_then(dimension) {
                for s in 0..4 {
                    out.push(Decl::BorderWidth(s, b.clone()));
                    out.push(Decl::BorderStyle(s, BorderStyle::Solid));
                }
            }
            for (a, sides) in [("hspace", [1u8, 3]), ("vspace", [0, 2])] {
                if let Some(d) = attr(a).and_then(dimension) {
                    for s in sides {
                        out.push(Decl::Margin(s, SLPA::L(d.clone())));
                    }
                }
            }
            match attr("align")
                .map(|a| a.trim().to_ascii_lowercase())
                .as_deref()
            {
                Some("left") => out.push(Decl::Float(Float::Left)),
                Some("right") => out.push(Decl::Float(Float::Right)),
                Some("top") => out.push(Decl::VerticalAlign(SVAlign::K(VAlign::Top))),
                Some("middle") | Some("absmiddle") | Some("center") => {
                    out.push(Decl::VerticalAlign(SVAlign::K(VAlign::Middle)))
                }
                Some("bottom") | Some("baseline") => {
                    out.push(Decl::VerticalAlign(SVAlign::K(VAlign::Baseline)))
                }
                _ => {}
            }
        }
        FONT => {
            if let Some(c) = attr("color").and_then(color_hint) {
                out.push(Decl::Color(c));
            }
            if let Some(f) = attr("face") {
                let toks = crate::css::tokenizer::tokenize(f, 256);
                let mut tmp = Vec::new();
                let pcx = ParseCx {
                    resolve: &|s: &str| String::from(s),
                };
                if parse_declaration("font-family", &toks, &pcx, &mut tmp) {
                    out.extend(tmp);
                }
            }
            if let Some(sz) = attr("size") {
                // HTML §15.3.6 "legacy font size": 1..7, relative with + or -.
                let t = sz.trim();
                let (rel, digits) = match t.as_bytes().first() {
                    Some(b'+') => (1, &t[1..]),
                    Some(b'-') => (-1, &t[1..]),
                    _ => (0, t),
                };
                if let Ok(n) = digits.trim().parse::<i32>() {
                    let v = match rel {
                        1 => 3 + n,
                        -1 => 3 - n,
                        _ => n,
                    }
                    .clamp(1, 7);
                    let px = [10.0, 13.0, 16.0, 18.0, 24.0, 32.0, 48.0][(v - 1) as usize];
                    out.push(Decl::FontSize(SFontSize::L(values::Len::px(px))));
                }
            }
        }
        HR => {
            if let Some(w) = attr("width").and_then(dimension) {
                out.push(Decl::Width(SSize::L(w)));
            }
            if let Some(sz) = attr("size").and_then(|s| s.trim().parse::<f32>().ok()) {
                out.push(Decl::Height(SSize::L(values::Len::px((sz - 2.0).max(0.0)))));
            }
            if let Some(c) = attr("color").and_then(color_hint) {
                for s in 0..4 {
                    out.push(Decl::BorderColor(s, c));
                    out.push(Decl::BorderStyle(s, BorderStyle::Solid));
                }
                out.push(Decl::BackgroundColor(c));
            } else if e.has_attr("noshade") {
                for s in 0..4 {
                    out.push(Decl::BorderStyle(s, BorderStyle::Solid));
                    out.push(Decl::BorderColor(s, SColor::Rgba(0xFF80_8080)));
                }
                out.push(Decl::BackgroundColor(SColor::Rgba(0xFF80_8080)));
            }
            match attr("align")
                .map(|a| a.trim().to_ascii_lowercase())
                .as_deref()
            {
                Some("left") => {
                    out.push(Decl::Margin(3, SLPA::L(values::Len::px(0.0))));
                    out.push(Decl::Margin(1, SLPA::Auto));
                }
                Some("right") => {
                    out.push(Decl::Margin(1, SLPA::L(values::Len::px(0.0))));
                    out.push(Decl::Margin(3, SLPA::Auto));
                }
                _ => {}
            }
        }
        DIV | P | H1 | H2 | H3 | H4 | H5 | H6 | CAPTION | LEGEND => {
            if let Some(a) = attr("align") {
                align_hint(a, out);
            }
        }
        UL | OL | LI => {
            if let Some(t) = attr("type") {
                let ty = match t.trim() {
                    "1" => Some(ListStyleType::Decimal),
                    "a" => Some(ListStyleType::LowerAlpha),
                    "A" => Some(ListStyleType::UpperAlpha),
                    "i" => Some(ListStyleType::LowerRoman),
                    "I" => Some(ListStyleType::UpperRoman),
                    x => match x.to_ascii_lowercase().as_str() {
                        "disc" => Some(ListStyleType::Disc),
                        "circle" => Some(ListStyleType::Circle),
                        "square" => Some(ListStyleType::Square),
                        "none" => Some(ListStyleType::None),
                        _ => None,
                    },
                };
                if let Some(ty) = ty {
                    out.push(Decl::ListStyleType(ty, None));
                }
            }
        }
        TEXTAREA | INPUT | SELECT | BUTTON => {}
        _ => {}
    }
    if e.has_attr("hidden") && e.name != EMPTY {
        // [hidden] is in the UA sheet; until="found" etc. stay hidden too.
    }
    if let Some(d) = e.attr("dir")
        && d.eq_ignore_ascii_case("rtl")
    {
        // Right-to-left text is laid out left-to-right (no bidi), but aligned right.
        out.push(Decl::TextAlign(STextAlign::Right));
    }
    let _ = NodeData::Document;
}
