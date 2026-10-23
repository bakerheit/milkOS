//! Layout: the box tree ([`build`]), block and inline formatting contexts with floats and
//! margin collapsing ([`block`], [`inline`]), tables ([`table`]), flexbox ([`flex`]),
//! absolute and fixed positioning ([`position`]) and intrinsic sizes ([`intrinsic`]).
//!
//! Units: [`Au`], 1/64 of a device pixel (CSS px × zoom × 64), so rounding happens only when
//! the display list is built. Positions are absolute document coordinates of each box's
//! border box, before relative-position offsets (those are applied at paint).

pub mod block;
pub mod build;
pub mod flex;
pub mod inline;
pub mod intrinsic;
pub mod position;
pub mod table;

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use crate::api::{ControlId, ControlKind, Font, FontMetrics, ImageId, TextMeasurer};
use crate::dom::{Dom, NodeId};
use crate::fmath;
use crate::style::cascade::Styles;
use crate::style::types::*;

pub type Au = i32;
pub const AU: Au = 64;
pub type BoxId = u32;
pub const NO_BOX: BoxId = u32::MAX;
/// The largest single length (2^24 Au, about 262000 device px): anything a hostile page asks
/// for beyond that is clamped, so sums of lengths stay far from overflowing.
pub const MAX_LEN: Au = 1 << 24;

#[derive(Clone, Debug)]
pub enum Replaced {
    /// `<img>`, `<input type=image>`: the image (None: no src) and its intrinsic size (None
    /// until supplied, or failed).
    Image(Option<ImageId>, Option<(u32, u32)>),
    Control(ControlId, ControlKind),
    /// `<iframe>`, `<video>`, `<canvas>`, `<embed>`, `<object>`, `<svg>`: an empty box of the
    /// given default size (CSS px).
    Empty(f32, f32),
}

#[derive(Clone, Debug)]
pub enum Kind {
    /// A block container (block-level, or the inside of an inline-block / cell / flex item).
    Block,
    /// An inline box (display: inline); children are inline-level.
    Inline,
    /// Text (white-space not processed yet).
    Text(Rc<str>),
    /// `<br>`
    Break,
    /// `<wbr>`: a break opportunity.
    Wbr,
    Replaced(Replaced),
    /// A list marker's text (outside markers are positioned by the list item).
    Marker(Rc<str>),
    Table,
    RowGroup,
    Row,
    Cell {
        colspan: u32,
        rowspan: u32,
    },
    Caption,
    /// `<col>` / `<colgroup>` (span); only their widths are used.
    Column {
        span: u32,
    },
    Flex,
}

/// A laid-out line box of an inline formatting context.
#[derive(Clone, Debug, Default)]
pub struct Line {
    pub y: Au,
    pub h: Au,
    pub baseline: Au,
}

/// What an inline formatting context produced, for painting and hit testing.
#[derive(Clone, Debug)]
pub enum Frag {
    /// Text: [`Ifc::text`] range, the box whose style it has, its font, pen position.
    Text {
        start: u32,
        end: u32,
        styled: BoxId,
        font: Font,
        x: Au,
        baseline: Au,
        w: Au,
        node: NodeId,
    },
    /// A piece of an inline box on one line (for its background, borders, links).
    Inline {
        b: BoxId,
        x: Au,
        y: Au,
        w: Au,
        h: Au,
        first: bool,
        last: bool,
    },
}

#[derive(Clone, Debug, Default)]
pub struct Ifc {
    pub text: String,
    pub lines: Vec<Line>,
    pub frags: Vec<Frag>,
}

#[derive(Clone, Debug)]
pub struct LBox {
    pub kind: Kind,
    pub style: Rc<Style>,
    /// The element (or text node) it is for; NONE for anonymous boxes.
    pub node: NodeId,
    pub parent: BoxId,
    pub children: Vec<BoxId>,
    /// Block containers: true if the children are inline-level (an IFC).
    pub inline_content: bool,
    /// Participates in a block formatting context as a block-level box (false: inline-level).
    pub block_level: bool,
    /// Out of flow: float or absolutely/fixed positioned.
    pub out_of_flow: bool,
    /// A list item's outside marker box.
    pub marker: BoxId,
    // ---- layout results ----
    /// Border box, document coordinates.
    pub x: Au,
    pub y: Au,
    pub w: Au,
    pub h: Au,
    pub margin: [Au; 4],
    pub border: [Au; 4],
    pub padding: [Au; 4],
    pub ifc: Option<alloc::boxed::Box<Ifc>>,
    pub first_baseline: Option<Au>,
    pub last_baseline: Option<Au>,
    /// For absolutely positioned boxes: the static position (where it would have been).
    pub static_pos: (Au, Au),
    pub laid_out: bool,
    /// The inputs of the last [`block::layout_sized`] (cb width, content width, forced
    /// height, cb height): the same inputs again reuse the result instead of redoing it.
    pub sized_key: Option<(Au, Au, Option<Au>, Option<Au>)>,
}

impl LBox {
    pub fn new(kind: Kind, style: Rc<Style>, node: NodeId) -> LBox {
        LBox {
            kind,
            style,
            node,
            parent: NO_BOX,
            children: Vec::new(),
            inline_content: false,
            block_level: false,
            out_of_flow: false,
            marker: NO_BOX,
            x: 0,
            y: 0,
            w: 0,
            h: 0,
            margin: [0; 4],
            border: [0; 4],
            padding: [0; 4],
            ifc: None,
            first_baseline: None,
            last_baseline: None,
            static_pos: (0, 0),
            laid_out: false,
            sized_key: None,
        }
    }

    pub fn is_inline_level(&self) -> bool {
        !self.block_level
    }

    /// Content box (x, y, w, h).
    pub fn content(&self) -> (Au, Au, Au, Au) {
        let x = self.x + self.border[3] + self.padding[3];
        let y = self.y + self.border[0] + self.padding[0];
        let w = self.w - self.border[1] - self.border[3] - self.padding[1] - self.padding[3];
        let h = self.h - self.border[0] - self.border[2] - self.padding[0] - self.padding[2];
        (x, y, w.max(0), h.max(0))
    }

    /// Padding box.
    pub fn padding_box(&self) -> (Au, Au, Au, Au) {
        (
            self.x + self.border[3],
            self.y + self.border[0],
            (self.w - self.border[1] - self.border[3]).max(0),
            (self.h - self.border[0] - self.border[2]).max(0),
        )
    }

    pub fn margin_box_h(&self) -> Au {
        self.h + self.margin[0] + self.margin[2]
    }

    pub fn margin_box_w(&self) -> Au {
        self.w + self.margin[1] + self.margin[3]
    }
}

/// Everything layout reads, plus caches.
pub struct Cx<'a> {
    pub dom: &'a Dom,
    pub m: &'a dyn TextMeasurer,
    /// Device px per CSS px.
    pub zoom: f32,
    /// Viewport, Au.
    pub vw: Au,
    pub vh: Au,
    metrics: RefCell<BTreeMap<Font, FontMetrics>>,
    spaces: RefCell<BTreeMap<Font, Au>>,
    pub intrinsic: RefCell<BTreeMap<BoxId, (Au, Au)>>,
    /// Flex items' content heights at a width (box, width, cb height) → height.
    pub heights: RefCell<BTreeMap<(BoxId, Au, Option<Au>), Au>>,
    /// Absolutely / fixed positioned boxes in tree order (laid out after the flow).
    pub abs: RefCell<Vec<BoxId>>,
    /// Work counter (bounded on hostile pages).
    pub work: core::cell::Cell<u64>,
}

impl<'a> Cx<'a> {
    pub fn new(dom: &'a Dom, m: &'a dyn TextMeasurer, zoom: f32, vw: Au, vh: Au) -> Cx<'a> {
        Cx {
            dom,
            m,
            zoom,
            vw,
            vh,
            metrics: RefCell::new(BTreeMap::new()),
            spaces: RefCell::new(BTreeMap::new()),
            intrinsic: RefCell::new(BTreeMap::new()),
            heights: RefCell::new(BTreeMap::new()),
            abs: RefCell::new(Vec::new()),
            work: core::cell::Cell::new(0),
        }
    }

    /// CSS px → Au.
    pub fn au(&self, px: f32) -> Au {
        fmath::round_i32(px * self.zoom * AU as f32).clamp(-MAX_LEN, MAX_LEN)
    }

    /// A length-percentage against a reference in Au.
    pub fn lp(&self, l: &LP, base: Au) -> Au {
        let px = self.au(l.px);
        if l.pct == 0.0 {
            return px;
        }
        px.saturating_add(fmath::round_i32(l.pct * base as f32 / 100.0))
            .clamp(-MAX_LEN, MAX_LEN)
    }

    pub fn font(&self, s: &Style) -> Font {
        let size = fmath::round_i32(s.font_size * self.zoom).clamp(1, 2000) as u16;
        Font {
            family: s.family,
            size,
            weight: s.font_weight,
            italic: s.italic,
        }
    }

    pub fn metrics(&self, f: &Font) -> FontMetrics {
        if let Some(m) = self.metrics.borrow().get(f) {
            return *m;
        }
        let m = self.m.metrics(f);
        self.metrics.borrow_mut().insert(*f, m);
        m
    }

    pub fn text_width(&self, f: &Font, s: &str) -> Au {
        self.work.set(self.work.get() + 1);
        self.m.text_width(f, s).saturating_mul(AU).clamp(0, MAX_LEN)
    }

    pub fn space_width(&self, f: &Font) -> Au {
        if let Some(w) = self.spaces.borrow().get(f) {
            return *w;
        }
        let w = self.text_width(f, " ");
        self.spaces.borrow_mut().insert(*f, w);
        w
    }

    /// The used line height (Au) for a style.
    pub fn line_height(&self, s: &Style) -> Au {
        let f = self.font(s);
        match s.line_height {
            LineHeight::Normal => self.metrics(&f).line_height.saturating_mul(AU),
            LineHeight::Number(n) => self.au(n * s.font_size),
            LineHeight::Px(p) => self.au(p),
        }
    }
}

/// The result of layout: the box tree with positions.
pub struct Laid {
    pub boxes: Vec<LBox>,
    pub root: BoxId,
    /// Document size (scrollable overflow), Au.
    pub width: Au,
    pub height: Au,
}

/// Inputs for one layout.
pub struct Inputs<'a> {
    pub dom: &'a Dom,
    pub styles: &'a Styles,
    pub images: &'a build::ImageLookup<'a>,
    pub controls: &'a BTreeMap<NodeId, (ControlId, ControlKind)>,
}

/// Build the box tree and lay it out in the viewport `cx` describes.
pub fn layout(inp: &Inputs, cx: &Cx) -> Laid {
    let mut boxes = build::build(inp);
    let root = if boxes.is_empty() { NO_BOX } else { 0 };
    if root == NO_BOX {
        return Laid {
            boxes,
            root,
            width: cx.vw,
            height: 0,
        };
    }
    // The initial containing block: the viewport (CSS 2.1 §10.1).
    block::layout_root(&mut boxes, cx, root);
    position::layout_abs(&mut boxes, cx);
    let (w, h) = position::extent(&boxes, root);
    Laid {
        boxes,
        root,
        width: w.max(cx.vw),
        height: h.max(0),
    }
}
