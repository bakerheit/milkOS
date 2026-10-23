//! Computed values: the [`Style`] every element (and every pseudo-element and anonymous box)
//! gets. Lengths are CSS pixels as `f32` ([`LP`]: px plus a percentage resolved at layout).

use alloc::collections::BTreeMap;
use alloc::rc::Rc;

use crate::api::{BorderStyle, Color, Decoration, Family, ImageId};
use crate::css::tokenizer::Tok;
pub use crate::css::values::LP;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Display {
    None,
    Contents,
    Block,
    FlowRoot,
    Inline,
    InlineBlock,
    ListItem,
    Flex,
    InlineFlex,
    Grid,
    InlineGrid,
    Table,
    InlineTable,
    TableRowGroup,
    TableHeaderGroup,
    TableFooterGroup,
    TableRow,
    TableCell,
    TableColumn,
    TableColumnGroup,
    TableCaption,
}

impl Display {
    pub fn is_inline_level(self) -> bool {
        matches!(
            self,
            Display::Inline
                | Display::InlineBlock
                | Display::InlineFlex
                | Display::InlineTable
                | Display::InlineGrid
        )
    }
    /// CSS Display §2.7 "blockification".
    pub fn blockify(self) -> Display {
        match self {
            Display::Inline | Display::InlineBlock => Display::Block,
            Display::InlineFlex => Display::Flex,
            Display::InlineGrid => Display::Grid,
            Display::InlineTable => Display::Table,
            Display::TableRowGroup
            | Display::TableHeaderGroup
            | Display::TableFooterGroup
            | Display::TableRow
            | Display::TableCell
            | Display::TableColumn
            | Display::TableColumnGroup
            | Display::TableCaption => Display::Block,
            d => d,
        }
    }
    pub fn is_table_internal(self) -> bool {
        matches!(
            self,
            Display::TableRowGroup
                | Display::TableHeaderGroup
                | Display::TableFooterGroup
                | Display::TableRow
                | Display::TableCell
                | Display::TableColumn
                | Display::TableColumnGroup
                | Display::TableCaption
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Position {
    Static,
    Relative,
    Absolute,
    Fixed,
    /// Laid out as relative (no scroll-linked behaviour).
    Sticky,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Float {
    None,
    Left,
    Right,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Clear {
    None,
    Left,
    Right,
    Both,
}

/// width / height / flex-basis
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Size {
    Auto,
    L(LP),
    MinContent,
    MaxContent,
    FitContent,
    /// flex-basis: content
    Content,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum LPA {
    Auto,
    L(LP),
}

impl LPA {
    pub fn is_auto(&self) -> bool {
        matches!(self, LPA::Auto)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum LineHeight {
    Normal,
    Number(f32),
    Px(f32),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextAlign {
    Left,
    Right,
    Center,
    Justify,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextTransform {
    None,
    Uppercase,
    Lowercase,
    Capitalize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WhiteSpace {
    Normal,
    Pre,
    Nowrap,
    PreWrap,
    PreLine,
}

impl WhiteSpace {
    pub fn collapses_spaces(self) -> bool {
        matches!(
            self,
            WhiteSpace::Normal | WhiteSpace::Nowrap | WhiteSpace::PreLine
        )
    }
    pub fn keeps_newlines(self) -> bool {
        matches!(
            self,
            WhiteSpace::Pre | WhiteSpace::PreWrap | WhiteSpace::PreLine
        )
    }
    pub fn wraps(self) -> bool {
        matches!(
            self,
            WhiteSpace::Normal | WhiteSpace::PreWrap | WhiteSpace::PreLine
        )
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum VAlign {
    Baseline,
    Sub,
    Super,
    TextTop,
    TextBottom,
    Middle,
    Top,
    Bottom,
    L(LP),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Visibility {
    Visible,
    Hidden,
    Collapse,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Overflow {
    Visible,
    Hidden,
    Clip,
    Scroll,
    Auto,
}

impl Overflow {
    pub fn clips(self) -> bool {
        self != Overflow::Visible
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ListStyleType {
    None,
    Disc,
    Circle,
    Square,
    Decimal,
    DecimalLeadingZero,
    LowerAlpha,
    UpperAlpha,
    LowerRoman,
    UpperRoman,
    LowerGreek,
    /// list-style-type: "string"
    Str,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FlexDirection {
    Row,
    RowReverse,
    Column,
    ColumnReverse,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FlexWrap {
    NoWrap,
    Wrap,
    WrapReverse,
}

/// justify-content / align-content
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Justify {
    Normal,
    FlexStart,
    FlexEnd,
    Center,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
    Stretch,
}

/// align-items / align-self
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Align {
    Auto,
    Normal,
    FlexStart,
    FlexEnd,
    Center,
    Baseline,
    Stretch,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BgRepeat {
    Repeat,
    RepeatX,
    RepeatY,
    NoRepeat,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum BgSize {
    Auto,
    Cover,
    Contain,
    /// width, height (Auto = keep the ratio)
    L(LPA, LPA),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BoxSizing {
    ContentBox,
    BorderBox,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cursor {
    Auto,
    Pointer,
    Text,
    Default,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ContentItem {
    Str(Rc<str>),
    Attr(Rc<str>),
    OpenQuote,
    CloseQuote,
    /// counter(name, style)
    Counter(Rc<str>, ListStyleType),
}

/// Custom properties in effect (`--name: tokens`), inherited, shared between elements.
pub type CustomMap = BTreeMap<Rc<str>, Rc<[Tok]>>;

/// One element's computed style.
#[derive(Clone, Debug)]
pub struct Style {
    // ---- inherited ----
    pub color: Color,
    pub family: Family,
    /// CSS px
    pub font_size: f32,
    pub font_weight: u16,
    pub italic: bool,
    pub small_caps: bool,
    pub line_height: LineHeight,
    pub text_align: TextAlign,
    pub text_indent: LP,
    pub text_transform: TextTransform,
    pub white_space: WhiteSpace,
    pub visibility: Visibility,
    /// `color-scheme` (inherited): bit 1 light, 2 dark, 4 only; 0 = normal.
    pub color_scheme: u8,
    pub list_style_type: ListStyleType,
    pub list_style_string: Option<Rc<str>>,
    pub list_inside: bool,
    pub border_collapse: bool,
    /// horizontal, vertical, CSS px
    pub border_spacing: (f32, f32),
    pub caption_bottom: bool,
    pub cursor: Cursor,
    pub break_anywhere: bool,
    pub letter_spacing: f32,
    pub word_spacing: f32,
    pub custom: Option<Rc<CustomMap>>,
    /// Text decorations in effect (propagated from ancestors, CSS Text Decoration §2.1).
    pub deco_in_effect: Decoration,
    pub deco_color_in_effect: Color,
    pub quotes_depth: u8,
    // ---- not inherited ----
    pub display: Display,
    /// What `display` was before blockification (an inline-block in a flex container is
    /// still atomic for its own children).
    pub original_display: Display,
    pub position: Position,
    pub float: Float,
    pub clear: Clear,
    pub inset: [LPA; 4],
    pub width: Size,
    pub height: Size,
    pub min_width: Size,
    pub min_height: Size,
    pub max_width: Option<LP>,
    pub max_height: Option<LP>,
    pub margin: [LPA; 4],
    pub padding: [LP; 4],
    /// Border widths, CSS px, already 0 for style none/hidden.
    pub border_width: [f32; 4],
    pub border_style: [BorderStyle; 4],
    pub border_color: [Color; 4],
    pub border_radius: LP,
    pub box_sizing: BoxSizing,
    pub background_color: Color,
    pub bg_image: Option<ImageId>,
    pub bg_repeat: BgRepeat,
    pub bg_position: (LP, LP),
    pub bg_size: BgSize,
    pub overflow_x: Overflow,
    pub overflow_y: Overflow,
    pub vertical_align: VAlign,
    pub z_index: Option<i32>,
    pub opacity: f32,
    pub deco_line: Decoration,
    pub deco_color: Option<Color>,
    pub content: Option<Rc<[ContentItem]>>,
    pub flex_direction: FlexDirection,
    pub flex_wrap: FlexWrap,
    pub justify_content: Justify,
    pub align_items: Align,
    pub align_self: Align,
    pub align_content: Justify,
    pub flex_grow: f32,
    pub flex_shrink: f32,
    pub flex_basis: Size,
    pub order: i32,
    pub row_gap: LP,
    pub column_gap: LP,
    pub table_fixed: bool,
    pub aspect_ratio: Option<f32>,
    /// counter-reset / counter-increment (name, value), only the first of each.
    pub counter_reset: Option<(Rc<str>, i32)>,
    pub counter_increment: Option<(Rc<str>, i32)>,
    /// Grid properties, when any is set (on containers or items).
    pub grid: Option<Rc<GridStyle>>,
}

impl Style {
    /// The initial values (CSS's "initial value" column), with the UA's default font.
    pub fn initial() -> Style {
        Style {
            color: 0xFF00_0000,
            family: Family::Serif,
            font_size: 16.0,
            font_weight: 400,
            italic: false,
            small_caps: false,
            line_height: LineHeight::Normal,
            text_align: TextAlign::Left,
            text_indent: LP::ZERO,
            text_transform: TextTransform::None,
            white_space: WhiteSpace::Normal,
            visibility: Visibility::Visible,
            color_scheme: 0,
            list_style_type: ListStyleType::Disc,
            list_style_string: None,
            list_inside: false,
            border_collapse: false,
            border_spacing: (0.0, 0.0),
            caption_bottom: false,
            cursor: Cursor::Auto,
            break_anywhere: false,
            letter_spacing: 0.0,
            word_spacing: 0.0,
            custom: None,
            deco_in_effect: Decoration::NONE,
            deco_color_in_effect: 0xFF00_0000,
            quotes_depth: 0,
            display: Display::Inline,
            original_display: Display::Inline,
            position: Position::Static,
            float: Float::None,
            clear: Clear::None,
            inset: [LPA::Auto; 4],
            width: Size::Auto,
            height: Size::Auto,
            min_width: Size::Auto,
            min_height: Size::Auto,
            max_width: None,
            max_height: None,
            margin: [LPA::L(LP::ZERO); 4],
            padding: [LP::ZERO; 4],
            border_width: [0.0; 4],
            border_style: [BorderStyle::None; 4],
            border_color: [0xFF00_0000; 4],
            border_radius: LP::ZERO,
            box_sizing: BoxSizing::ContentBox,
            background_color: 0,
            bg_image: None,
            bg_repeat: BgRepeat::Repeat,
            bg_position: (LP::ZERO, LP::ZERO),
            bg_size: BgSize::Auto,
            overflow_x: Overflow::Visible,
            overflow_y: Overflow::Visible,
            vertical_align: VAlign::Baseline,
            z_index: None,
            opacity: 1.0,
            deco_line: Decoration::NONE,
            deco_color: None,
            content: None,
            flex_direction: FlexDirection::Row,
            flex_wrap: FlexWrap::NoWrap,
            justify_content: Justify::Normal,
            align_items: Align::Normal,
            align_self: Align::Auto,
            align_content: Justify::Normal,
            flex_grow: 0.0,
            flex_shrink: 1.0,
            flex_basis: Size::Auto,
            order: 0,
            row_gap: LP::ZERO,
            column_gap: LP::ZERO,
            table_fixed: false,
            aspect_ratio: None,
            counter_reset: None,
            counter_increment: None,
            grid: None,
        }
    }

    /// A child's starting point: the inherited properties from `p`, the rest initial.
    pub fn inherit(p: &Style, init: &Style) -> Style {
        let mut s = init.clone();
        s.color = p.color;
        s.family = p.family;
        s.font_size = p.font_size;
        s.font_weight = p.font_weight;
        s.italic = p.italic;
        s.small_caps = p.small_caps;
        s.line_height = p.line_height;
        s.text_align = p.text_align;
        s.text_indent = p.text_indent;
        s.text_transform = p.text_transform;
        s.white_space = p.white_space;
        s.visibility = p.visibility;
        s.color_scheme = p.color_scheme;
        s.list_style_type = p.list_style_type;
        s.list_style_string = p.list_style_string.clone();
        s.list_inside = p.list_inside;
        s.border_collapse = p.border_collapse;
        s.border_spacing = p.border_spacing;
        s.caption_bottom = p.caption_bottom;
        s.cursor = p.cursor;
        s.break_anywhere = p.break_anywhere;
        s.letter_spacing = p.letter_spacing;
        s.word_spacing = p.word_spacing;
        s.custom = p.custom.clone();
        s.deco_in_effect = p.deco_in_effect;
        s.deco_color_in_effect = p.deco_color_in_effect;
        s.quotes_depth = p.quotes_depth;
        s
    }

    pub fn is_positioned(&self) -> bool {
        self.position != Position::Static
    }

    pub fn is_out_of_flow_positioned(&self) -> bool {
        matches!(self.position, Position::Absolute | Position::Fixed)
    }

    /// The used line height in CSS px for this element's font (`normal` from the metrics).
    pub fn line_height_px(&self, normal: f32) -> f32 {
        match self.line_height {
            LineHeight::Normal => normal,
            LineHeight::Number(n) => n * self.font_size,
            LineHeight::Px(p) => p,
        }
    }

    pub fn establishes_bfc(&self) -> bool {
        self.float != Float::None
            || self.is_out_of_flow_positioned()
            || self.overflow_x.clips()
            || self.overflow_y.clips()
            || matches!(
                self.display,
                Display::FlowRoot
                    | Display::InlineBlock
                    | Display::TableCell
                    | Display::TableCaption
                    | Display::Flex
                    | Display::InlineFlex
                    | Display::Grid
                    | Display::InlineGrid
            )
    }
}

// ---- CSS Grid ------------------------------------------------------------------------------

/// A track sizing function's minimum (CSS Grid 1 §7.2.3).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum TMin {
    Fixed(LP),
    Auto,
    MinContent,
    MaxContent,
}

/// A track sizing function's maximum.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum TMax {
    Fixed(LP),
    Fr(f32),
    Auto,
    MinContent,
    MaxContent,
    /// fit-content(limit)
    FitContent(LP),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Track {
    pub min: TMin,
    pub max: TMax,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AutoRepeat {
    Fill,
    Fit,
}

/// `grid-template-columns` / `-rows`: explicit tracks, with an optional auto-repeat inserted
/// at `at` (`repeat(auto-fill, ...)`).
#[derive(Clone, PartialEq, Debug, Default)]
pub struct TrackList {
    pub tracks: alloc::vec::Vec<Track>,
    pub auto_repeat: Option<(AutoRepeat, usize, alloc::vec::Vec<Track>)>,
}

/// A named grid area: (name, row start, column start, row end, column end).
pub type Area = (Rc<str>, u16, u16, u16, u16);

/// One end of `grid-column` / `grid-row`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GridLine {
    Auto,
    /// A line number (1-based; negative counts from the end).
    Line(i32),
    Span(u32),
}

/// The grid-specific computed values (boxed in [`Style`]: most elements have none).
#[derive(Clone, PartialEq, Debug)]
pub struct GridStyle {
    pub columns: TrackList,
    pub rows: TrackList,
    pub auto_rows: Track,
    pub auto_columns: Track,
    pub column_flow: bool,
    pub col: (GridLine, GridLine),
    pub row: (GridLine, GridLine),
    /// grid-template-areas: (name, row start, col start, row end, col end), 0-based, end
    /// exclusive; and the area grid's size (rows, columns).
    pub areas: Rc<[Area]>,
    pub area_size: (u16, u16),
    /// grid-area: NAME on an item.
    pub area_name: Option<Rc<str>>,
}

impl Default for GridStyle {
    fn default() -> Self {
        GridStyle {
            columns: TrackList::default(),
            rows: TrackList::default(),
            auto_rows: Track {
                min: TMin::Auto,
                max: TMax::Auto,
            },
            auto_columns: Track {
                min: TMin::Auto,
                max: TMax::Auto,
            },
            column_flow: false,
            col: (GridLine::Auto, GridLine::Auto),
            row: (GridLine::Auto, GridLine::Auto),
            areas: Rc::from(alloc::vec::Vec::new()),
            area_size: (0, 0),
            area_name: None,
        }
    }
}
