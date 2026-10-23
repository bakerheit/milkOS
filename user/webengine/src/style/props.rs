//! Property declarations: parsing a `name: value` into specified longhand [`Decl`]s, with the
//! shorthands expanded (CSS 2.1 and the relevant level 3/4 modules). Values with `var()` are
//! kept as [`Decl::Pending`] and parsed after substitution, at cascade time (CSS Variables
//! §3). Unknown properties and invalid values produce nothing (the declaration is dropped).

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use super::types::*;
use crate::api::{BorderStyle, Decoration, Family};
use crate::css::tokenizer::Tok;
use crate::css::values::{self, Len, SColor, components, fn_args, split_commas, trim};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wide {
    Inherit,
    Initial,
    Unset,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SLPA {
    Auto,
    L(Len),
}

#[derive(Clone, Debug, PartialEq)]
pub enum SSize {
    Auto,
    L(Len),
    MinContent,
    MaxContent,
    FitContent,
    Content,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SFontSize {
    /// em and % are relative to the parent's font size.
    L(Len),
    Smaller,
    Larger,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SWeight {
    Abs(u16),
    Bolder,
    Lighter,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SLineHeight {
    Normal,
    Number(f32),
    L(Len),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum STextAlign {
    Left,
    Right,
    Center,
    Justify,
    Start,
    End,
    MatchParent,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SVAlign {
    K(VAlign),
    L(Len),
}

#[derive(Clone, Debug, PartialEq)]
pub enum SBgSize {
    Auto,
    Cover,
    Contain,
    L(SLPA, SLPA),
}

/// A specified longhand declaration.
#[derive(Clone, Debug, PartialEq)]
pub enum Decl {
    Display(Display),
    Position(Position),
    Float(Float),
    Clear(Clear),
    /// side: 0 top, 1 right, 2 bottom, 3 left (all the 4-sided properties)
    Inset(u8, SLPA),
    Width(SSize),
    Height(SSize),
    MinWidth(SSize),
    MinHeight(SSize),
    MaxWidth(Option<Len>),
    MaxHeight(Option<Len>),
    Margin(u8, SLPA),
    Padding(u8, Len),
    BorderWidth(u8, Len),
    BorderStyle(u8, BorderStyle),
    BorderColor(u8, SColor),
    BorderRadius(Len),
    BoxSizing(BoxSizing),
    Color(SColor),
    BackgroundColor(SColor),
    /// Absolute URL (None = `none`).
    BackgroundImage(Option<Rc<str>>),
    BackgroundRepeat(BgRepeat),
    BackgroundPosition(Len, Len),
    BackgroundSize(SBgSize),
    FontFamily(Family),
    FontSize(SFontSize),
    FontWeight(SWeight),
    FontStyle(bool),
    FontVariant(bool),
    LineHeight(SLineHeight),
    TextAlign(STextAlign),
    TextIndent(Len),
    TextTransform(TextTransform),
    WhiteSpace(WhiteSpace),
    TextDecorationLine(Decoration),
    TextDecorationColor(SColor),
    VerticalAlign(SVAlign),
    Visibility(Visibility),
    OverflowX(Overflow),
    OverflowY(Overflow),
    ListStyleType(ListStyleType, Option<Rc<str>>),
    ListStylePosition(bool),
    BorderCollapse(bool),
    BorderSpacing(Len, Len),
    CaptionSide(bool),
    TableLayout(bool),
    Cursor(Cursor),
    BreakAnywhere(bool),
    LetterSpacing(Option<Len>),
    WordSpacing(Option<Len>),
    ZIndex(Option<i32>),
    Opacity(f32),
    Content(Option<Rc<[ContentItem]>>),
    FlexDirection(FlexDirection),
    FlexWrap(FlexWrap),
    JustifyContent(Justify),
    AlignItems(Align),
    AlignSelf(Align),
    AlignContent(Justify),
    FlexGrow(f32),
    FlexShrink(f32),
    FlexBasis(SSize),
    Order(i32),
    RowGap(Len),
    ColumnGap(Len),
    AspectRatio(Option<f32>),
    CounterReset(Option<(Rc<str>, i32)>),
    CounterIncrement(Option<(Rc<str>, i32)>),
    /// A CSS-wide keyword for one longhand.
    Wide(Prop, Wide),
    /// `name: value` with `var()` in it, parsed after substitution.
    Pending(Rc<str>, Rc<[Tok]>),
    /// `--name: value`
    Custom(Rc<str>, Rc<[Tok]>),
}

/// Longhand ids (for the cascade's "last declaration wins" table and the CSS-wide keywords).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Prop {
    Display,
    Position,
    Float,
    Clear,
    Inset0,
    Inset1,
    Inset2,
    Inset3,
    Width,
    Height,
    MinWidth,
    MinHeight,
    MaxWidth,
    MaxHeight,
    Margin0,
    Margin1,
    Margin2,
    Margin3,
    Padding0,
    Padding1,
    Padding2,
    Padding3,
    BorderWidth0,
    BorderWidth1,
    BorderWidth2,
    BorderWidth3,
    BorderStyle0,
    BorderStyle1,
    BorderStyle2,
    BorderStyle3,
    BorderColor0,
    BorderColor1,
    BorderColor2,
    BorderColor3,
    BorderRadius,
    BoxSizing,
    Color,
    BackgroundColor,
    BackgroundImage,
    BackgroundRepeat,
    BackgroundPosition,
    BackgroundSize,
    FontFamily,
    FontSize,
    FontWeight,
    FontStyle,
    FontVariant,
    LineHeight,
    TextAlign,
    TextIndent,
    TextTransform,
    WhiteSpace,
    TextDecorationLine,
    TextDecorationColor,
    VerticalAlign,
    Visibility,
    OverflowX,
    OverflowY,
    ListStyleType,
    ListStylePosition,
    BorderCollapse,
    BorderSpacing,
    CaptionSide,
    TableLayout,
    Cursor,
    BreakAnywhere,
    LetterSpacing,
    WordSpacing,
    ZIndex,
    Opacity,
    Content,
    FlexDirection,
    FlexWrap,
    JustifyContent,
    AlignItems,
    AlignSelf,
    AlignContent,
    FlexGrow,
    FlexShrink,
    FlexBasis,
    Order,
    RowGap,
    ColumnGap,
    AspectRatio,
    CounterReset,
    CounterIncrement,
    Count,
}

pub const PROP_COUNT: usize = Prop::Count as usize;

fn side(base: Prop, s: u8) -> Prop {
    // The four sides are consecutive.
    let v = base as u8 + s.min(3);
    // SAFETY-free conversion: look it up.
    PROPS_BY_ID[v as usize]
}

static PROPS_BY_ID: [Prop; PROP_COUNT] = {
    // Built from the discriminants (const context).
    let mut a = [Prop::Display; PROP_COUNT];
    let all = [
        Prop::Display,
        Prop::Position,
        Prop::Float,
        Prop::Clear,
        Prop::Inset0,
        Prop::Inset1,
        Prop::Inset2,
        Prop::Inset3,
        Prop::Width,
        Prop::Height,
        Prop::MinWidth,
        Prop::MinHeight,
        Prop::MaxWidth,
        Prop::MaxHeight,
        Prop::Margin0,
        Prop::Margin1,
        Prop::Margin2,
        Prop::Margin3,
        Prop::Padding0,
        Prop::Padding1,
        Prop::Padding2,
        Prop::Padding3,
        Prop::BorderWidth0,
        Prop::BorderWidth1,
        Prop::BorderWidth2,
        Prop::BorderWidth3,
        Prop::BorderStyle0,
        Prop::BorderStyle1,
        Prop::BorderStyle2,
        Prop::BorderStyle3,
        Prop::BorderColor0,
        Prop::BorderColor1,
        Prop::BorderColor2,
        Prop::BorderColor3,
        Prop::BorderRadius,
        Prop::BoxSizing,
        Prop::Color,
        Prop::BackgroundColor,
        Prop::BackgroundImage,
        Prop::BackgroundRepeat,
        Prop::BackgroundPosition,
        Prop::BackgroundSize,
        Prop::FontFamily,
        Prop::FontSize,
        Prop::FontWeight,
        Prop::FontStyle,
        Prop::FontVariant,
        Prop::LineHeight,
        Prop::TextAlign,
        Prop::TextIndent,
        Prop::TextTransform,
        Prop::WhiteSpace,
        Prop::TextDecorationLine,
        Prop::TextDecorationColor,
        Prop::VerticalAlign,
        Prop::Visibility,
        Prop::OverflowX,
        Prop::OverflowY,
        Prop::ListStyleType,
        Prop::ListStylePosition,
        Prop::BorderCollapse,
        Prop::BorderSpacing,
        Prop::CaptionSide,
        Prop::TableLayout,
        Prop::Cursor,
        Prop::BreakAnywhere,
        Prop::LetterSpacing,
        Prop::WordSpacing,
        Prop::ZIndex,
        Prop::Opacity,
        Prop::Content,
        Prop::FlexDirection,
        Prop::FlexWrap,
        Prop::JustifyContent,
        Prop::AlignItems,
        Prop::AlignSelf,
        Prop::AlignContent,
        Prop::FlexGrow,
        Prop::FlexShrink,
        Prop::FlexBasis,
        Prop::Order,
        Prop::RowGap,
        Prop::ColumnGap,
        Prop::AspectRatio,
        Prop::CounterReset,
        Prop::CounterIncrement,
    ];
    let mut i = 0;
    while i < all.len() {
        a[all[i] as usize] = all[i];
        i += 1;
    }
    a
};

impl Decl {
    /// The longhand this sets (None for Pending/Custom).
    pub fn prop(&self) -> Option<Prop> {
        use Decl as D;
        Some(match self {
            D::Display(_) => Prop::Display,
            D::Position(_) => Prop::Position,
            D::Float(_) => Prop::Float,
            D::Clear(_) => Prop::Clear,
            D::Inset(s, _) => side(Prop::Inset0, *s),
            D::Width(_) => Prop::Width,
            D::Height(_) => Prop::Height,
            D::MinWidth(_) => Prop::MinWidth,
            D::MinHeight(_) => Prop::MinHeight,
            D::MaxWidth(_) => Prop::MaxWidth,
            D::MaxHeight(_) => Prop::MaxHeight,
            D::Margin(s, _) => side(Prop::Margin0, *s),
            D::Padding(s, _) => side(Prop::Padding0, *s),
            D::BorderWidth(s, _) => side(Prop::BorderWidth0, *s),
            D::BorderStyle(s, _) => side(Prop::BorderStyle0, *s),
            D::BorderColor(s, _) => side(Prop::BorderColor0, *s),
            D::BorderRadius(_) => Prop::BorderRadius,
            D::BoxSizing(_) => Prop::BoxSizing,
            D::Color(_) => Prop::Color,
            D::BackgroundColor(_) => Prop::BackgroundColor,
            D::BackgroundImage(_) => Prop::BackgroundImage,
            D::BackgroundRepeat(_) => Prop::BackgroundRepeat,
            D::BackgroundPosition(..) => Prop::BackgroundPosition,
            D::BackgroundSize(_) => Prop::BackgroundSize,
            D::FontFamily(_) => Prop::FontFamily,
            D::FontSize(_) => Prop::FontSize,
            D::FontWeight(_) => Prop::FontWeight,
            D::FontStyle(_) => Prop::FontStyle,
            D::FontVariant(_) => Prop::FontVariant,
            D::LineHeight(_) => Prop::LineHeight,
            D::TextAlign(_) => Prop::TextAlign,
            D::TextIndent(_) => Prop::TextIndent,
            D::TextTransform(_) => Prop::TextTransform,
            D::WhiteSpace(_) => Prop::WhiteSpace,
            D::TextDecorationLine(_) => Prop::TextDecorationLine,
            D::TextDecorationColor(_) => Prop::TextDecorationColor,
            D::VerticalAlign(_) => Prop::VerticalAlign,
            D::Visibility(_) => Prop::Visibility,
            D::OverflowX(_) => Prop::OverflowX,
            D::OverflowY(_) => Prop::OverflowY,
            D::ListStyleType(..) => Prop::ListStyleType,
            D::ListStylePosition(_) => Prop::ListStylePosition,
            D::BorderCollapse(_) => Prop::BorderCollapse,
            D::BorderSpacing(..) => Prop::BorderSpacing,
            D::CaptionSide(_) => Prop::CaptionSide,
            D::TableLayout(_) => Prop::TableLayout,
            D::Cursor(_) => Prop::Cursor,
            D::BreakAnywhere(_) => Prop::BreakAnywhere,
            D::LetterSpacing(_) => Prop::LetterSpacing,
            D::WordSpacing(_) => Prop::WordSpacing,
            D::ZIndex(_) => Prop::ZIndex,
            D::Opacity(_) => Prop::Opacity,
            D::Content(_) => Prop::Content,
            D::FlexDirection(_) => Prop::FlexDirection,
            D::FlexWrap(_) => Prop::FlexWrap,
            D::JustifyContent(_) => Prop::JustifyContent,
            D::AlignItems(_) => Prop::AlignItems,
            D::AlignSelf(_) => Prop::AlignSelf,
            D::AlignContent(_) => Prop::AlignContent,
            D::FlexGrow(_) => Prop::FlexGrow,
            D::FlexShrink(_) => Prop::FlexShrink,
            D::FlexBasis(_) => Prop::FlexBasis,
            D::Order(_) => Prop::Order,
            D::RowGap(_) => Prop::RowGap,
            D::ColumnGap(_) => Prop::ColumnGap,
            D::AspectRatio(_) => Prop::AspectRatio,
            D::CounterReset(_) => Prop::CounterReset,
            D::CounterIncrement(_) => Prop::CounterIncrement,
            D::Wide(p, _) => *p,
            D::Pending(..) | D::Custom(..) => return None,
        })
    }
}

/// Which longhands a property name covers (for CSS-wide keywords on shorthands).
fn longhands(name: &str) -> &'static [Prop] {
    use Prop::*;
    match name {
        "margin" => &[Margin0, Margin1, Margin2, Margin3],
        "padding" => &[Padding0, Padding1, Padding2, Padding3],
        "inset" => &[Inset0, Inset1, Inset2, Inset3],
        "border" => &[
            BorderWidth0,
            BorderWidth1,
            BorderWidth2,
            BorderWidth3,
            BorderStyle0,
            BorderStyle1,
            BorderStyle2,
            BorderStyle3,
            BorderColor0,
            BorderColor1,
            BorderColor2,
            BorderColor3,
        ],
        "border-width" => &[BorderWidth0, BorderWidth1, BorderWidth2, BorderWidth3],
        "border-style" => &[BorderStyle0, BorderStyle1, BorderStyle2, BorderStyle3],
        "border-color" => &[BorderColor0, BorderColor1, BorderColor2, BorderColor3],
        "border-top" => &[BorderWidth0, BorderStyle0, BorderColor0],
        "border-right" => &[BorderWidth1, BorderStyle1, BorderColor1],
        "border-bottom" => &[BorderWidth2, BorderStyle2, BorderColor2],
        "border-left" => &[BorderWidth3, BorderStyle3, BorderColor3],
        "background" => &[
            BackgroundColor,
            BackgroundImage,
            BackgroundRepeat,
            BackgroundPosition,
            BackgroundSize,
        ],
        "font" => &[
            FontFamily,
            FontSize,
            FontWeight,
            FontStyle,
            FontVariant,
            LineHeight,
        ],
        "list-style" => &[ListStyleType, ListStylePosition],
        "flex" => &[FlexGrow, FlexShrink, FlexBasis],
        "flex-flow" => &[FlexDirection, FlexWrap],
        "overflow" => &[OverflowX, OverflowY],
        "gap" | "grid-gap" => &[RowGap, ColumnGap],
        "text-decoration" => &[TextDecorationLine, TextDecorationColor],
        "place-items" => &[AlignItems],
        "place-content" => &[AlignContent, JustifyContent],
        "place-self" => &[AlignSelf],
        _ => &[],
    }
}

/// Context for parsing values: how to make a URL absolute.
pub struct ParseCx<'a> {
    pub resolve: &'a dyn Fn(&str) -> String,
}

fn has_var(t: &[Tok]) -> bool {
    t.iter()
        .any(|x| matches!(x, Tok::Function(f) if f == "var" || f == "env"))
}

/// Parse one declaration into longhands appended to `out`. Returns false if the property is
/// unknown or the value invalid (nothing is appended then).
pub fn parse_declaration(name: &str, value: &[Tok], cx: &ParseCx, out: &mut Vec<Decl>) -> bool {
    let value = trim(value);
    if let Some(custom) = name.strip_prefix("--") {
        let _ = custom;
        out.push(Decl::Custom(Rc::from(name), Rc::from(value)));
        return true;
    }
    let name = name.to_ascii_lowercase();
    if has_var(value) {
        out.push(Decl::Pending(Rc::from(name.as_str()), Rc::from(value)));
        return true;
    }
    // CSS-wide keywords.
    if value.len() == 1
        && let Tok::Ident(k) = &value[0]
    {
        let w = match k.to_ascii_lowercase().as_str() {
            "inherit" => Some(Wide::Inherit),
            "initial" => Some(Wide::Initial),
            "unset" | "revert" | "revert-layer" => Some(Wide::Unset),
            _ => None,
        };
        if let Some(w) = w {
            let lh = longhands(&name);
            if !lh.is_empty() {
                for p in lh {
                    out.push(Decl::Wide(*p, w));
                }
                return true;
            }
            let mut tmp = Vec::new();
            // A longhand: find its id by parsing a valid dummy value.
            if let Some(p) = longhand_id(&name, cx, &mut tmp) {
                out.push(Decl::Wide(p, w));
                return true;
            }
            return false;
        }
    }
    let start = out.len();
    let ok = parse_value(&name, value, cx, out).is_some();
    if !ok {
        out.truncate(start);
    }
    ok
}

/// The longhand a property name is, found by the parser itself on an "initial"-ish value.
fn longhand_id(name: &str, cx: &ParseCx, tmp: &mut Vec<Decl>) -> Option<Prop> {
    for probe in ["0", "auto", "none", "normal", "black", "1"] {
        tmp.clear();
        let toks = crate::css::tokenizer::tokenize(probe, 8);
        if parse_value(name, &toks, cx, tmp).is_some() && tmp.len() == 1 {
            return tmp[0].prop();
        }
    }
    None
}

fn ident(c: &[Tok]) -> Option<String> {
    match c {
        [Tok::Ident(s)] => Some(s.to_ascii_lowercase()),
        _ => None,
    }
}

/// The single-component value of a longhand (None if there are several components).
fn single(v: &[Tok]) -> Option<&[Tok]> {
    let c = components(v);
    if c.len() == 1 { Some(c[0]) } else { None }
}

fn slpa(c: &[Tok]) -> Option<SLPA> {
    if ident(c).as_deref() == Some("auto") {
        return Some(SLPA::Auto);
    }
    values::length(c, false).map(SLPA::L)
}

fn nonneg_len(c: &[Tok]) -> Option<Len> {
    let l = values::length(c, false)?;
    if let Len::Lin(s) = &l
        && (s.px < 0.0 || s.pct < 0.0 || s.em < 0.0 || s.rem < 0.0)
        && s.px + s.em + s.rem + s.pct + s.vw + s.vh < 0.0
    {
        return None;
    }
    Some(l)
}

fn ssize(c: &[Tok]) -> Option<SSize> {
    match ident(c).as_deref() {
        Some("auto") => return Some(SSize::Auto),
        Some("min-content") | Some("-webkit-min-content") | Some("-moz-min-content") => {
            return Some(SSize::MinContent);
        }
        Some("max-content") | Some("-webkit-max-content") | Some("-moz-max-content") => {
            return Some(SSize::MaxContent);
        }
        Some("fit-content") | Some("-webkit-fit-content") | Some("-moz-fit-content") => {
            return Some(SSize::FitContent);
        }
        Some("-webkit-fill-available")
        | Some("-moz-available")
        | Some("stretch")
        | Some("fill-available") => {
            return Some(SSize::Auto);
        }
        Some("content") => return Some(SSize::Content),
        _ => {}
    }
    if let [Tok::Function(f), ..] = c
        && f == "fit-content"
    {
        return Some(SSize::FitContent);
    }
    nonneg_len(c).map(SSize::L)
}

fn border_style(c: &[Tok]) -> Option<BorderStyle> {
    Some(match ident(c)?.as_str() {
        "none" | "hidden" => BorderStyle::None,
        "solid" => BorderStyle::Solid,
        "dashed" => BorderStyle::Dashed,
        "dotted" => BorderStyle::Dotted,
        "double" => BorderStyle::Double,
        "groove" | "ridge" | "inset" | "outset" => BorderStyle::Shaded,
        _ => return None,
    })
}

fn border_width(c: &[Tok]) -> Option<Len> {
    match ident(c).as_deref() {
        Some("thin") => Some(Len::px(1.0)),
        Some("medium") => Some(Len::px(3.0)),
        Some("thick") => Some(Len::px(5.0)),
        _ => nonneg_len(c),
    }
}

/// 1 to 4 values → top, right, bottom, left (CSS 2.1 §8.3 "margin" shorthand rule).
fn four<T: Clone>(c: &[&[Tok]], f: impl Fn(&[Tok]) -> Option<T>) -> Option<[T; 4]> {
    let v: Vec<T> = c.iter().map(|x| f(x)).collect::<Option<Vec<T>>>()?;
    Some(match v.len() {
        1 => [v[0].clone(), v[0].clone(), v[0].clone(), v[0].clone()],
        2 => [v[0].clone(), v[1].clone(), v[0].clone(), v[1].clone()],
        3 => [v[0].clone(), v[1].clone(), v[2].clone(), v[1].clone()],
        4 => [v[0].clone(), v[1].clone(), v[2].clone(), v[3].clone()],
        _ => return None,
    })
}

fn url_of(c: &[Tok], cx: &ParseCx) -> Option<Option<Rc<str>>> {
    match c.first()? {
        Tok::Url(u) => Some(Some(Rc::from((cx.resolve)(u).as_str()))),
        Tok::Function(f) if f == "url" => {
            let a = trim(fn_args(c));
            match a.first() {
                Some(Tok::Str(u)) => Some(Some(Rc::from((cx.resolve)(u).as_str()))),
                _ => None,
            }
        }
        Tok::Ident(n) if n.eq_ignore_ascii_case("none") => Some(None),
        // Gradients and image-set(): no image (the background colour shows).
        Tok::Function(f)
            if f.ends_with("gradient")
                || f == "image-set"
                || f == "-webkit-image-set"
                || f == "cross-fade" =>
        {
            Some(None)
        }
        _ => None,
    }
}

/// A family name list → the generic family it lands on (see [`Family`]).
fn font_family(v: &[Tok]) -> Option<Family> {
    let mut result = None;
    for part in split_commas(v) {
        let name: String = match part {
            [Tok::Str(s)] => s.to_ascii_lowercase(),
            _ => {
                let mut n = String::new();
                for t in part {
                    match t {
                        Tok::Ident(s) => {
                            if !n.is_empty() {
                                n.push(' ');
                            }
                            n.push_str(&s.to_ascii_lowercase());
                        }
                        Tok::Whitespace => {}
                        _ => return None,
                    }
                }
                n
            }
        };
        if name.is_empty() {
            return None;
        }
        if result.is_none() {
            result = family_of(&name);
        }
    }
    Some(result.unwrap_or(Family::Serif))
}

fn family_of(n: &str) -> Option<Family> {
    Some(match n {
        "serif" | "times" | "times new roman" | "georgia" | "garamond" | "palatino"
        | "palatino linotype" | "book antiqua" | "cambria" | "baskerville" | "linux libertine"
        | "libertinus serif" | "noto serif" | "dejavu serif" | "liberation serif" | "charter"
        | "bitstream charter" | "source serif pro" | "merriweather" | "ui-serif" | "new york"
        | "iowan old style" | "constantia" | "lora" | "pt serif" => Family::Serif,
        "sans-serif"
        | "arial"
        | "helvetica"
        | "helvetica neue"
        | "verdana"
        | "tahoma"
        | "trebuchet ms"
        | "open sans"
        | "roboto"
        | "lato"
        | "noto sans"
        | "dejavu sans"
        | "liberation sans"
        | "inter"
        | "source sans pro"
        | "segoe ui"
        | "ubuntu"
        | "cantarell"
        | "fira sans"
        | "ibm plex sans"
        | "figtree"
        | "atkinson hyperlegible"
        | "lucida grande"
        | "lucida sans unicode"
        | "geneva"
        | "calibri"
        | "montserrat"
        | "poppins"
        | "nunito"
        | "raleway"
        | "work sans"
        | "pt sans"
        | "oxygen"
        | "arimo"
        | "gill sans"
        | "avenir"
        | "avenir next"
        | "futura"
        | "optima"
        | "candara"
        | "franklin gothic medium"
        | "noto sans display"
        | "droid sans" => Family::SansSerif,
        "monospace"
        | "courier"
        | "courier new"
        | "menlo"
        | "monaco"
        | "consolas"
        | "lucida console"
        | "dejavu sans mono"
        | "liberation mono"
        | "source code pro"
        | "fira code"
        | "fira mono"
        | "ibm plex mono"
        | "sf mono"
        | "ui-monospace"
        | "roboto mono"
        | "ubuntu mono"
        | "jetbrains mono"
        | "andale mono"
        | "noto mono"
        | "noto sans mono"
        | "cascadia code"
        | "inconsolata"
        | "droid sans mono"
        | "lucida sans typewriter"
        | "sfmono-regular" => Family::Monospace,
        "cursive" | "comic sans ms" | "brush script mt" | "apple chancery" => Family::Cursive,
        "fantasy" | "impact" | "papyrus" => Family::Fantasy,
        "system-ui" | "-apple-system" | "blinkmacsystemfont" | "ui-sans-serif" | "sf pro text"
        | "sf pro display" | ".sfnstext-regular" | "san francisco" => Family::SystemUi,
        _ => return None,
    })
}

fn font_size_kw(k: &str) -> Option<SFontSize> {
    let px = match k {
        "xx-small" => 9.0,
        "x-small" => 10.0,
        "small" => 13.0,
        "medium" => 16.0,
        "large" => 18.0,
        "x-large" => 24.0,
        "xx-large" => 32.0,
        "xxx-large" => 48.0,
        "smaller" => return Some(SFontSize::Smaller),
        "larger" => return Some(SFontSize::Larger),
        _ => return None,
    };
    Some(SFontSize::L(Len::px(px)))
}

fn font_size(c: &[Tok]) -> Option<SFontSize> {
    if let Some(k) = ident(c) {
        return font_size_kw(&k);
    }
    nonneg_len(c).map(SFontSize::L)
}

fn font_weight(c: &[Tok]) -> Option<SWeight> {
    if let Some(k) = ident(c) {
        return Some(match k.as_str() {
            "normal" => SWeight::Abs(400),
            "bold" => SWeight::Abs(700),
            "bolder" => SWeight::Bolder,
            "lighter" => SWeight::Lighter,
            _ => return None,
        });
    }
    let n = values::number(c)?;
    if !(1.0..=1000.0).contains(&n) {
        return None;
    }
    Some(SWeight::Abs(n as u16))
}

fn line_height(c: &[Tok]) -> Option<SLineHeight> {
    if ident(c).as_deref() == Some("normal") {
        return Some(SLineHeight::Normal);
    }
    if let Some(n) = values::number(c) {
        return if n >= 0.0 {
            Some(SLineHeight::Number(n))
        } else {
            None
        };
    }
    nonneg_len(c).map(SLineHeight::L)
}

fn list_style_type(c: &[Tok]) -> Option<(ListStyleType, Option<Rc<str>>)> {
    if let [Tok::Str(s)] = c {
        return Some((ListStyleType::Str, Some(Rc::from(s.as_str()))));
    }
    Some((
        match ident(c)?.as_str() {
            "none" => ListStyleType::None,
            "disc" => ListStyleType::Disc,
            "circle" => ListStyleType::Circle,
            "square" => ListStyleType::Square,
            "decimal" => ListStyleType::Decimal,
            "decimal-leading-zero" => ListStyleType::DecimalLeadingZero,
            "lower-alpha" | "lower-latin" => ListStyleType::LowerAlpha,
            "upper-alpha" | "upper-latin" => ListStyleType::UpperAlpha,
            "lower-roman" => ListStyleType::LowerRoman,
            "upper-roman" => ListStyleType::UpperRoman,
            "lower-greek" => ListStyleType::LowerGreek,
            // Other counter styles: decimal.
            "armenian" | "georgian" | "hebrew" | "cjk-ideographic" | "hiragana" | "katakana"
            | "arabic-indic" | "devanagari" | "cjk-decimal" => ListStyleType::Decimal,
            "disclosure-open" | "disclosure-closed" => ListStyleType::Disc,
            _ => return None,
        },
        None,
    ))
}

fn justify(k: &str) -> Option<Justify> {
    Some(match k {
        "normal" => Justify::Normal,
        "flex-start" | "start" | "left" | "self-start" => Justify::FlexStart,
        "flex-end" | "end" | "right" | "self-end" => Justify::FlexEnd,
        "center" => Justify::Center,
        "space-between" => Justify::SpaceBetween,
        "space-around" => Justify::SpaceAround,
        "space-evenly" => Justify::SpaceEvenly,
        "stretch" => Justify::Stretch,
        _ => return None,
    })
}

fn align(k: &str) -> Option<Align> {
    Some(match k {
        "auto" => Align::Auto,
        "normal" => Align::Normal,
        "flex-start" | "start" | "self-start" | "left" => Align::FlexStart,
        "flex-end" | "end" | "self-end" | "right" => Align::FlexEnd,
        "center" => Align::Center,
        "baseline" | "first" | "last" => Align::Baseline,
        "stretch" => Align::Stretch,
        _ => return None,
    })
}

/// The last ident of a multi-keyword alignment value (`safe center`, `first baseline`).
fn align_ident(v: &[Tok]) -> Option<String> {
    let c = components(v);
    let mut last = None;
    for x in c {
        let k = ident(x)?;
        if k != "safe" && k != "unsafe" && k != "legacy" {
            last = Some(k);
        }
    }
    last
}

fn bg_position_axis(c: &[Tok], horizontal: bool) -> Option<Len> {
    if let Some(k) = ident(c) {
        return Some(Len::Lin(values::SLen::pct(
            match (k.as_str(), horizontal) {
                ("left", true) | ("top", false) => 0.0,
                ("center", _) => 50.0,
                ("right", true) | ("bottom", false) => 100.0,
                _ => return None,
            },
        )));
    }
    values::length(c, false)
}

fn bg_position(c: &[&[Tok]]) -> Option<(Len, Len)> {
    let center = || Len::Lin(values::SLen::pct(50.0));
    match c.len() {
        1 => {
            let k = ident(c[0]);
            if matches!(k.as_deref(), Some("top") | Some("bottom")) {
                return Some((center(), bg_position_axis(c[0], false)?));
            }
            Some((bg_position_axis(c[0], true)?, center()))
        }
        2 => {
            let (a, b) = (ident(c[0]), ident(c[1]));
            let swap = matches!(a.as_deref(), Some("top") | Some("bottom"))
                || matches!(b.as_deref(), Some("left") | Some("right"));
            if swap {
                Some((
                    bg_position_axis(c[1], true)?,
                    bg_position_axis(c[0], false)?,
                ))
            } else {
                Some((
                    bg_position_axis(c[0], true)?,
                    bg_position_axis(c[1], false)?,
                ))
            }
        }
        // 3/4-value forms with edge offsets: keep the keywords only (approximate).
        3 | 4 => {
            let mut x = None;
            let mut y = None;
            for p in c {
                if let Some(k) = ident(p) {
                    match k.as_str() {
                        "left" | "right" => x = bg_position_axis(p, true),
                        "top" | "bottom" => y = bg_position_axis(p, false),
                        "center" => {}
                        _ => return None,
                    }
                }
            }
            Some((x.unwrap_or_else(center), y.unwrap_or_else(center)))
        }
        _ => None,
    }
}

fn bg_size(c: &[&[Tok]]) -> Option<SBgSize> {
    match c.len() {
        1 => match ident(c[0]).as_deref() {
            Some("cover") => Some(SBgSize::Cover),
            Some("contain") => Some(SBgSize::Contain),
            Some("auto") => Some(SBgSize::Auto),
            _ => Some(SBgSize::L(slpa(c[0])?, SLPA::Auto)),
        },
        2 => Some(SBgSize::L(slpa(c[0])?, slpa(c[1])?)),
        _ => None,
    }
}

fn bg_repeat(c: &[&[Tok]]) -> Option<BgRepeat> {
    let k: Vec<String> = c.iter().map(|x| ident(x)).collect::<Option<Vec<_>>>()?;
    let rep = |s: &str| matches!(s, "repeat" | "space" | "round");
    Some(match k.len() {
        1 => match k[0].as_str() {
            "repeat-x" => BgRepeat::RepeatX,
            "repeat-y" => BgRepeat::RepeatY,
            "no-repeat" => BgRepeat::NoRepeat,
            s if rep(s) => BgRepeat::Repeat,
            _ => return None,
        },
        2 => match (rep(&k[0]), rep(&k[1])) {
            (true, true) => BgRepeat::Repeat,
            (true, false) => BgRepeat::RepeatX,
            (false, true) => BgRepeat::RepeatY,
            (false, false) => BgRepeat::NoRepeat,
        },
        _ => return None,
    })
}

fn is_repeat_kw(k: &str) -> bool {
    matches!(
        k,
        "repeat" | "repeat-x" | "repeat-y" | "no-repeat" | "space" | "round"
    )
}

fn is_position_kw(k: &str) -> bool {
    matches!(k, "left" | "right" | "top" | "bottom" | "center")
}

/// `background` (CSS Backgrounds 3 §3.10), the final layer's colour and the first layer's
/// image, repeat, position and size.
fn background(v: &[Tok], cx: &ParseCx, out: &mut Vec<Decl>) -> Option<()> {
    let layers = split_commas(v);
    let mut color = SColor::Rgba(0);
    let mut image = None;
    let mut repeat = BgRepeat::Repeat;
    let mut pos = (Len::px(0.0), Len::px(0.0));
    let mut size = SBgSize::Auto;
    for (li, layer) in layers.iter().enumerate() {
        let c = components(layer);
        let mut i = 0;
        let first = li == 0;
        let mut pos_parts: Vec<&[Tok]> = Vec::new();
        let mut rep_parts: Vec<&[Tok]> = Vec::new();
        while i < c.len() {
            let x = c[i];
            if let Some(u) = url_of(x, cx) {
                if first {
                    image = u;
                }
                i += 1;
                continue;
            }
            if let Some(col) = values::color(x) {
                if li + 1 != layers.len() {
                    return None;
                }
                color = col;
                i += 1;
                continue;
            }
            if matches!(x.first(), Some(Tok::Delim('/'))) {
                // size after the position
                let mut sp = Vec::new();
                i += 1;
                while i < c.len() && sp.len() < 2 {
                    let k = ident(c[i]);
                    let is_size =
                        matches!(k.as_deref(), Some("cover") | Some("contain") | Some("auto"))
                            || (k.is_none() && values::length(c[i], false).is_some());
                    if !is_size {
                        break;
                    }
                    sp.push(c[i]);
                    i += 1;
                }
                if first {
                    size = bg_size(&sp)?;
                }
                continue;
            }
            if let Some(k) = ident(x) {
                if is_repeat_kw(&k) {
                    rep_parts.push(x);
                    i += 1;
                    continue;
                }
                if is_position_kw(&k) {
                    pos_parts.push(x);
                    i += 1;
                    continue;
                }
                if matches!(
                    k.as_str(),
                    "scroll"
                        | "fixed"
                        | "local"
                        | "border-box"
                        | "padding-box"
                        | "content-box"
                        | "text"
                ) {
                    i += 1;
                    continue;
                }
                return None;
            }
            if values::length(x, false).is_some() {
                pos_parts.push(x);
                i += 1;
                continue;
            }
            return None;
        }
        if first {
            if !rep_parts.is_empty() {
                repeat = bg_repeat(&rep_parts)?;
            }
            if !pos_parts.is_empty() {
                pos = bg_position(&pos_parts)?;
            }
        }
    }
    out.push(Decl::BackgroundColor(color));
    out.push(Decl::BackgroundImage(image));
    out.push(Decl::BackgroundRepeat(repeat));
    out.push(Decl::BackgroundPosition(pos.0, pos.1));
    out.push(Decl::BackgroundSize(size));
    Some(())
}

/// `border`, `border-top`, ...: width, style and colour in any order.
fn border_parts(v: &[Tok]) -> Option<(Len, BorderStyle, SColor)> {
    let mut w = None;
    let mut s = None;
    let mut c = None;
    for x in components(v) {
        if s.is_none()
            && let Some(st) = border_style(x)
        {
            s = Some(st);
        } else if w.is_none()
            && let Some(bw) = border_width(x)
        {
            w = Some(bw);
        } else if c.is_none()
            && let Some(col) = values::color(x)
        {
            c = Some(col);
        } else {
            return None;
        }
    }
    Some((
        w.unwrap_or(Len::px(3.0)),
        s.unwrap_or(BorderStyle::None),
        c.unwrap_or(SColor::CurrentColor),
    ))
}

/// `font` (CSS Fonts 4 §2.8).
fn font(v: &[Tok], out: &mut Vec<Decl>) -> Option<()> {
    let c = components(v);
    if c.len() == 1
        && let Some(k) = ident(c[0])
        && matches!(
            k.as_str(),
            "caption" | "icon" | "menu" | "message-box" | "small-caption" | "status-bar"
        )
    {
        out.push(Decl::FontFamily(Family::SystemUi));
        out.push(Decl::FontSize(SFontSize::L(Len::px(13.0))));
        out.push(Decl::FontWeight(SWeight::Abs(400)));
        out.push(Decl::FontStyle(false));
        out.push(Decl::FontVariant(false));
        out.push(Decl::LineHeight(SLineHeight::Normal));
        return Some(());
    }
    let mut italic = false;
    let mut small_caps = false;
    let mut weight = SWeight::Abs(400);
    let mut i = 0;
    // Style, variant, weight, stretch, in any order, before the size.
    while i < c.len() {
        let x = c[i];
        if let Some(k) = ident(x) {
            match k.as_str() {
                "normal" => {}
                "italic" | "oblique" => italic = true,
                "small-caps" => small_caps = true,
                "bold" | "bolder" | "lighter" => weight = font_weight(x)?,
                "ultra-condensed" | "extra-condensed" | "condensed" | "semi-condensed"
                | "semi-expanded" | "expanded" | "extra-expanded" | "ultra-expanded" => {}
                _ => break,
            }
            i += 1;
            continue;
        }
        if let [Tok::Number { .. }] = x
            && let Some(w) = font_weight(x)
        {
            weight = w;
            i += 1;
            continue;
        }
        break;
    }
    let size = font_size(c.get(i)?)?;
    i += 1;
    let mut lh = SLineHeight::Normal;
    if matches!(c.get(i).and_then(|x| x.first()), Some(Tok::Delim('/'))) {
        lh = line_height(c.get(i + 1)?)?;
        i += 2;
    }
    // The rest is the family list (original tokens from the first family component).
    let rest_start = {
        let first = c.get(i)?;
        let off = first.as_ptr() as usize - v.as_ptr() as usize;
        off / core::mem::size_of::<Tok>()
    };
    let family = font_family(&v[rest_start..])?;
    out.push(Decl::FontStyle(italic));
    out.push(Decl::FontVariant(small_caps));
    out.push(Decl::FontWeight(weight));
    out.push(Decl::FontSize(size));
    out.push(Decl::LineHeight(lh));
    out.push(Decl::FontFamily(family));
    Some(())
}

fn flex(v: &[Tok], out: &mut Vec<Decl>) -> Option<()> {
    let c = components(v);
    let (g, s, b) = match c.len() {
        1 => match ident(c[0]).as_deref() {
            Some("none") => (0.0, 0.0, SSize::Auto),
            Some("auto") => (1.0, 1.0, SSize::Auto),
            Some("initial") => (0.0, 1.0, SSize::Auto),
            _ => {
                if let Some(n) = values::number(c[0]) {
                    (n, 1.0, SSize::L(Len::px(0.0)))
                } else {
                    (1.0, 1.0, ssize(c[0])?)
                }
            }
        },
        2 => {
            let g = values::number(c[0])?;
            if let Some(s) = values::number(c[1]) {
                (g, s, SSize::L(Len::px(0.0)))
            } else {
                (g, 1.0, ssize(c[1])?)
            }
        }
        3 => (values::number(c[0])?, values::number(c[1])?, ssize(c[2])?),
        _ => return None,
    };
    if g < 0.0 || s < 0.0 {
        return None;
    }
    out.push(Decl::FlexGrow(g));
    out.push(Decl::FlexShrink(s));
    out.push(Decl::FlexBasis(b));
    Some(())
}

fn flex_direction(k: &str) -> Option<FlexDirection> {
    Some(match k {
        "row" => FlexDirection::Row,
        "row-reverse" => FlexDirection::RowReverse,
        "column" => FlexDirection::Column,
        "column-reverse" => FlexDirection::ColumnReverse,
        _ => return None,
    })
}

fn flex_wrap(k: &str) -> Option<FlexWrap> {
    Some(match k {
        "nowrap" => FlexWrap::NoWrap,
        "wrap" => FlexWrap::Wrap,
        "wrap-reverse" => FlexWrap::WrapReverse,
        _ => return None,
    })
}

fn overflow(k: &str) -> Option<Overflow> {
    Some(match k {
        "visible" => Overflow::Visible,
        "hidden" => Overflow::Hidden,
        "clip" => Overflow::Clip,
        "scroll" => Overflow::Scroll,
        "auto" | "overlay" => Overflow::Auto,
        _ => return None,
    })
}

fn display(v: &[Tok]) -> Option<Display> {
    let c = components(v);
    let ks: Vec<String> = c.iter().map(|x| ident(x)).collect::<Option<Vec<_>>>()?;
    let ks: Vec<&str> = ks.iter().map(|s| s.as_str()).collect();
    // CSS Display 3 two-value syntax and the legacy keywords.
    Some(match ks.as_slice() {
        ["none"] => Display::None,
        ["contents"] => Display::Contents,
        ["block"] | ["block", "flow"] => Display::Block,
        ["flow-root"] | ["block", "flow-root"] => Display::FlowRoot,
        ["inline"] | ["inline", "flow"] => Display::Inline,
        ["inline-block"] | ["inline", "flow-root"] => Display::InlineBlock,
        ["list-item"] | ["block", "list-item"] | ["list-item", "block"] | ["list-item", "flow"] => {
            Display::ListItem
        }
        ["inline", "list-item"] | ["list-item", "inline"] => Display::Inline,
        ["flex"] | ["block", "flex"] | ["-webkit-box"] | ["-webkit-flex"] | ["-ms-flexbox"] => {
            Display::Flex
        }
        ["inline-flex"] | ["inline", "flex"] | ["-webkit-inline-box"] | ["-webkit-inline-flex"] => {
            Display::InlineFlex
        }
        // Grid is laid out as block flow (items stack); an honest fallback for now.
        ["grid"] | ["block", "grid"] | ["-ms-grid"] => Display::FlowRoot,
        ["inline-grid"] | ["inline", "grid"] => Display::InlineBlock,
        ["table"] | ["block", "table"] => Display::Table,
        ["inline-table"] | ["inline", "table"] => Display::InlineTable,
        ["table-row-group"] => Display::TableRowGroup,
        ["table-header-group"] => Display::TableHeaderGroup,
        ["table-footer-group"] => Display::TableFooterGroup,
        ["table-row"] => Display::TableRow,
        ["table-cell"] => Display::TableCell,
        ["table-column"] => Display::TableColumn,
        ["table-column-group"] => Display::TableColumnGroup,
        ["table-caption"] => Display::TableCaption,
        ["ruby"]
        | ["ruby-base"]
        | ["ruby-text"]
        | ["ruby-base-container"]
        | ["ruby-text-container"] => Display::Inline,
        _ => return None,
    })
}

fn content(v: &[Tok]) -> Option<Option<Rc<[ContentItem]>>> {
    let c = components(v);
    if c.len() == 1 && matches!(ident(c[0]).as_deref(), Some("none") | Some("normal")) {
        return Some(None);
    }
    let mut items = Vec::new();
    for x in c {
        match x {
            [Tok::Str(s)] => items.push(ContentItem::Str(Rc::from(s.as_str()))),
            [Tok::Ident(k)] => match k.to_ascii_lowercase().as_str() {
                "open-quote" => items.push(ContentItem::OpenQuote),
                "close-quote" => items.push(ContentItem::CloseQuote),
                "no-open-quote" | "no-close-quote" => {}
                _ => return None,
            },
            [Tok::Function(f), ..] if f == "attr" => {
                let a = trim(fn_args(x));
                let Some(Tok::Ident(n)) = a.first() else {
                    return None;
                };
                items.push(ContentItem::Attr(Rc::from(n.to_ascii_lowercase().as_str())));
            }
            [Tok::Function(f), ..] if f == "counter" || f == "counters" => {
                let a = split_commas(fn_args(x));
                let Some([Tok::Ident(n)]) = a.first().copied() else {
                    return None;
                };
                let style = a
                    .get(if f == "counters" { 2 } else { 1 })
                    .and_then(|s| list_style_type(s))
                    .map(|s| s.0)
                    .unwrap_or(ListStyleType::Decimal);
                items.push(ContentItem::Counter(Rc::from(n.as_str()), style));
            }
            [Tok::Url(_)] => {}
            [Tok::Function(f), ..] if f == "url" || f.ends_with("gradient") => {}
            [Tok::Delim('/')] => break, // alt text follows
            _ => return None,
        }
    }
    Some(Some(Rc::from(items)))
}

fn counter(v: &[Tok], default: i32) -> Option<Option<(Rc<str>, i32)>> {
    let c = components(v);
    if c.len() == 1 && ident(c[0]).as_deref() == Some("none") {
        return Some(None);
    }
    let [Tok::Ident(name)] = c.first()? else {
        return None;
    };
    let val = c.get(1).and_then(|x| values::integer(x)).unwrap_or(default);
    Some(Some((Rc::from(name.as_str()), val)))
}

fn deco_line(v: &[Tok]) -> Option<Decoration> {
    let mut d = 0u8;
    for x in components(v) {
        match ident(x)?.as_str() {
            "none" => {}
            "underline" => d |= Decoration::UNDERLINE.0,
            "overline" => d |= Decoration::OVERLINE.0,
            "line-through" => d |= Decoration::LINE_THROUGH.0,
            "blink" => {}
            _ => return None,
        }
    }
    Some(Decoration(d))
}

fn white_space(k: &str) -> Option<WhiteSpace> {
    Some(match k {
        "normal" => WhiteSpace::Normal,
        "pre" => WhiteSpace::Pre,
        "nowrap" => WhiteSpace::Nowrap,
        "pre-wrap" | "break-spaces" => WhiteSpace::PreWrap,
        "pre-line" => WhiteSpace::PreLine,
        _ => return None,
    })
}

/// The longhand and shorthand table.
fn parse_value(name: &str, v: &[Tok], cx: &ParseCx, out: &mut Vec<Decl>) -> Option<()> {
    let one = || single(v);
    let kw = || one().and_then(ident);
    let d = match name {
        "display" => Decl::Display(display(v)?),
        "position" => Decl::Position(match kw()?.as_str() {
            "static" => Position::Static,
            "relative" => Position::Relative,
            "absolute" => Position::Absolute,
            "fixed" => Position::Fixed,
            "sticky" | "-webkit-sticky" => Position::Sticky,
            _ => return None,
        }),
        "float" => Decl::Float(match kw()?.as_str() {
            "none" => Float::None,
            "left" | "inline-start" => Float::Left,
            "right" | "inline-end" => Float::Right,
            _ => return None,
        }),
        "clear" => Decl::Clear(match kw()?.as_str() {
            "none" => Clear::None,
            "left" | "inline-start" => Clear::Left,
            "right" | "inline-end" => Clear::Right,
            "both" => Clear::Both,
            _ => return None,
        }),
        "top" => Decl::Inset(0, slpa(one()?)?),
        "right" => Decl::Inset(1, slpa(one()?)?),
        "bottom" => Decl::Inset(2, slpa(one()?)?),
        "left" => Decl::Inset(3, slpa(one()?)?),
        "inset" => {
            let s = four(&components(v), slpa)?;
            for (i, x) in s.into_iter().enumerate() {
                out.push(Decl::Inset(i as u8, x));
            }
            return Some(());
        }
        "width" | "inline-size" => Decl::Width(ssize(one()?)?),
        "height" | "block-size" => Decl::Height(ssize(one()?)?),
        "min-width" | "min-inline-size" => Decl::MinWidth(ssize(one()?)?),
        "min-height" | "min-block-size" => Decl::MinHeight(ssize(one()?)?),
        "max-width" | "max-inline-size" | "max-height" | "max-block-size" => {
            let c = one()?;
            let val = match ident(c).as_deref() {
                Some("none") => None,
                Some("min-content")
                | Some("max-content")
                | Some("fit-content")
                | Some("-webkit-fill-available")
                | Some("stretch") => None,
                _ => Some(nonneg_len(c)?),
            };
            if name.contains("width") || name.contains("inline") {
                Decl::MaxWidth(val)
            } else {
                Decl::MaxHeight(val)
            }
        }
        "margin" => {
            let s = four(&components(v), slpa)?;
            for (i, x) in s.into_iter().enumerate() {
                out.push(Decl::Margin(i as u8, x));
            }
            return Some(());
        }
        "margin-top" | "margin-block-start" => Decl::Margin(0, slpa(one()?)?),
        "margin-right" | "margin-inline-end" => Decl::Margin(1, slpa(one()?)?),
        "margin-bottom" | "margin-block-end" => Decl::Margin(2, slpa(one()?)?),
        "margin-left" | "margin-inline-start" => Decl::Margin(3, slpa(one()?)?),
        "margin-inline" | "margin-block" => {
            let c = components(v);
            let a = slpa(c.first()?)?;
            let b = if c.len() > 1 { slpa(c[1])? } else { a.clone() };
            let (s0, s1) = if name == "margin-inline" {
                (3, 1)
            } else {
                (0, 2)
            };
            out.push(Decl::Margin(s0, a));
            out.push(Decl::Margin(s1, b));
            return Some(());
        }
        "padding" => {
            let s = four(&components(v), nonneg_len)?;
            for (i, x) in s.into_iter().enumerate() {
                out.push(Decl::Padding(i as u8, x));
            }
            return Some(());
        }
        "padding-top" | "padding-block-start" => Decl::Padding(0, nonneg_len(one()?)?),
        "padding-right" | "padding-inline-end" => Decl::Padding(1, nonneg_len(one()?)?),
        "padding-bottom" | "padding-block-end" => Decl::Padding(2, nonneg_len(one()?)?),
        "padding-left" | "padding-inline-start" => Decl::Padding(3, nonneg_len(one()?)?),
        "padding-inline" | "padding-block" => {
            let c = components(v);
            let a = nonneg_len(c.first()?)?;
            let b = if c.len() > 1 {
                nonneg_len(c[1])?
            } else {
                a.clone()
            };
            let (s0, s1) = if name == "padding-inline" {
                (3, 1)
            } else {
                (0, 2)
            };
            out.push(Decl::Padding(s0, a));
            out.push(Decl::Padding(s1, b));
            return Some(());
        }
        "border"
        | "border-top"
        | "border-right"
        | "border-bottom"
        | "border-left"
        | "border-block-start"
        | "border-block-end"
        | "border-inline-start"
        | "border-inline-end" => {
            let (w, s, c) = border_parts(v)?;
            let sides: &[u8] = match name {
                "border" => &[0, 1, 2, 3],
                "border-top" | "border-block-start" => &[0],
                "border-right" | "border-inline-end" => &[1],
                "border-bottom" | "border-block-end" => &[2],
                _ => &[3],
            };
            for &i in sides {
                out.push(Decl::BorderWidth(i, w.clone()));
                out.push(Decl::BorderStyle(i, s));
                out.push(Decl::BorderColor(i, c));
            }
            return Some(());
        }
        "border-width" => {
            for (i, x) in four(&components(v), border_width)?.into_iter().enumerate() {
                out.push(Decl::BorderWidth(i as u8, x));
            }
            return Some(());
        }
        "border-style" => {
            for (i, x) in four(&components(v), border_style)?.into_iter().enumerate() {
                out.push(Decl::BorderStyle(i as u8, x));
            }
            return Some(());
        }
        "border-color" => {
            for (i, x) in four(&components(v), values::color)?.into_iter().enumerate() {
                out.push(Decl::BorderColor(i as u8, x));
            }
            return Some(());
        }
        "border-top-width" => Decl::BorderWidth(0, border_width(one()?)?),
        "border-right-width" => Decl::BorderWidth(1, border_width(one()?)?),
        "border-bottom-width" => Decl::BorderWidth(2, border_width(one()?)?),
        "border-left-width" => Decl::BorderWidth(3, border_width(one()?)?),
        "border-top-style" => Decl::BorderStyle(0, border_style(one()?)?),
        "border-right-style" => Decl::BorderStyle(1, border_style(one()?)?),
        "border-bottom-style" => Decl::BorderStyle(2, border_style(one()?)?),
        "border-left-style" => Decl::BorderStyle(3, border_style(one()?)?),
        "border-top-color" => Decl::BorderColor(0, values::color(one()?)?),
        "border-right-color" => Decl::BorderColor(1, values::color(one()?)?),
        "border-bottom-color" => Decl::BorderColor(2, values::color(one()?)?),
        "border-left-color" => Decl::BorderColor(3, values::color(one()?)?),
        "border-radius" => {
            // One uniform radius: the first value (elliptical / per-corner radii approximate).
            let c = components(v);
            Decl::BorderRadius(nonneg_len(c.first()?)?)
        }
        "border-top-left-radius"
        | "border-top-right-radius"
        | "border-bottom-left-radius"
        | "border-bottom-right-radius" => {
            // Accepted (so @supports and the cascade see them) but not drawn per corner.
            let c = components(v);
            nonneg_len(c.first()?)?;
            return Some(());
        }
        "box-sizing" | "-webkit-box-sizing" | "-moz-box-sizing" => {
            Decl::BoxSizing(match kw()?.as_str() {
                "content-box" => BoxSizing::ContentBox,
                "border-box" => BoxSizing::BorderBox,
                _ => return None,
            })
        }
        "color" => Decl::Color(values::color(one()?)?),
        "background-color" => Decl::BackgroundColor(values::color(one()?)?),
        "background-image" => {
            let layers = split_commas(v);
            Decl::BackgroundImage(url_of(layers.first()?, cx)?)
        }
        "background-repeat" => {
            Decl::BackgroundRepeat(bg_repeat(&components(split_commas(v).first()?))?)
        }
        "background-position" => {
            let p = bg_position(&components(split_commas(v).first()?))?;
            Decl::BackgroundPosition(p.0, p.1)
        }
        "background-size" => Decl::BackgroundSize(bg_size(&components(split_commas(v).first()?))?),
        "background" => {
            background(v, cx, out)?;
            return Some(());
        }
        "background-attachment"
        | "background-clip"
        | "background-origin"
        | "background-blend-mode" => {
            return Some(());
        }
        "font-family" => Decl::FontFamily(font_family(v)?),
        "font-size" => Decl::FontSize(font_size(one()?)?),
        "font-weight" => Decl::FontWeight(font_weight(one()?)?),
        "font-style" => Decl::FontStyle(
            match components(v).first().and_then(|c| ident(c))?.as_str() {
                "normal" => false,
                "italic" | "oblique" => true,
                _ => return None,
            },
        ),
        "font-variant" | "font-variant-caps" => Decl::FontVariant(match kw()?.as_str() {
            "small-caps" | "all-small-caps" | "petite-caps" | "all-petite-caps" => true,
            _ => false,
        }),
        "font" => {
            font(v, out)?;
            return Some(());
        }
        "line-height" => Decl::LineHeight(line_height(one()?)?),
        "text-align" => Decl::TextAlign(match kw()?.as_str() {
            "left" => STextAlign::Left,
            "right" => STextAlign::Right,
            "center" | "-webkit-center" | "-moz-center" => STextAlign::Center,
            "justify" | "justify-all" => STextAlign::Justify,
            "start" => STextAlign::Start,
            "end" => STextAlign::End,
            "match-parent" | "-webkit-match-parent" => STextAlign::MatchParent,
            _ => return None,
        }),
        "text-indent" => Decl::TextIndent(values::length(components(v).first()?, false)?),
        "text-transform" => Decl::TextTransform(match kw()?.as_str() {
            "none" => TextTransform::None,
            "uppercase" => TextTransform::Uppercase,
            "lowercase" => TextTransform::Lowercase,
            "capitalize" => TextTransform::Capitalize,
            _ => return None,
        }),
        "white-space" => {
            // Also the CSS Text 4 two-keyword forms ("normal nowrap", "preserve nowrap").
            let c = components(v);
            let ks: Vec<String> = c.iter().map(|x| ident(x)).collect::<Option<Vec<_>>>()?;
            if ks.len() == 1 {
                Decl::WhiteSpace(white_space(&ks[0])?)
            } else {
                let preserve = ks.iter().any(|k| k == "preserve" || k == "break-spaces");
                let nowrap = ks.iter().any(|k| k == "nowrap");
                Decl::WhiteSpace(match (preserve, nowrap) {
                    (true, true) => WhiteSpace::Pre,
                    (true, false) => WhiteSpace::PreWrap,
                    (false, true) => WhiteSpace::Nowrap,
                    (false, false) => WhiteSpace::Normal,
                })
            }
        }
        "text-wrap" | "text-wrap-mode" => match kw()?.as_str() {
            "nowrap" => Decl::WhiteSpace(WhiteSpace::Nowrap),
            _ => return Some(()),
        },
        "text-decoration" | "text-decoration-line" | "-webkit-text-decoration-line" => {
            if name == "text-decoration" {
                // line(s) style colour thickness, any order.
                let mut line = Decoration::NONE;
                let mut color = SColor::CurrentColor;
                for x in components(v) {
                    if let Some(l) = deco_line(x) {
                        line = Decoration(line.0 | l.0);
                    } else if let Some(c) = values::color(x) {
                        color = c;
                    } else if let Some(k) = ident(x) {
                        if !matches!(
                            k.as_str(),
                            "solid"
                                | "double"
                                | "dotted"
                                | "dashed"
                                | "wavy"
                                | "auto"
                                | "from-font"
                        ) {
                            return None;
                        }
                    } else if values::length(x, false).is_none() {
                        return None;
                    }
                }
                out.push(Decl::TextDecorationLine(line));
                out.push(Decl::TextDecorationColor(color));
                return Some(());
            }
            Decl::TextDecorationLine(deco_line(v)?)
        }
        "text-decoration-color" => Decl::TextDecorationColor(values::color(one()?)?),
        "text-decoration-style"
        | "text-decoration-thickness"
        | "text-underline-offset"
        | "text-underline-position" => {
            return Some(());
        }
        "vertical-align" => {
            let c = one()?;
            if let Some(k) = ident(c) {
                Decl::VerticalAlign(SVAlign::K(match k.as_str() {
                    "baseline" => VAlign::Baseline,
                    "sub" => VAlign::Sub,
                    "super" => VAlign::Super,
                    "text-top" => VAlign::TextTop,
                    "text-bottom" => VAlign::TextBottom,
                    "middle" => VAlign::Middle,
                    "top" => VAlign::Top,
                    "bottom" => VAlign::Bottom,
                    _ => return None,
                }))
            } else {
                Decl::VerticalAlign(SVAlign::L(values::length(c, false)?))
            }
        }
        "visibility" => Decl::Visibility(match kw()?.as_str() {
            "visible" => Visibility::Visible,
            "hidden" => Visibility::Hidden,
            "collapse" => Visibility::Collapse,
            _ => return None,
        }),
        "overflow" => {
            let c = components(v);
            let x = overflow(&ident(c.first()?)?)?;
            let y = if c.len() > 1 {
                overflow(&ident(c[1])?)?
            } else {
                x
            };
            out.push(Decl::OverflowX(x));
            out.push(Decl::OverflowY(y));
            return Some(());
        }
        "overflow-x" => Decl::OverflowX(overflow(&kw()?)?),
        "overflow-y" => Decl::OverflowY(overflow(&kw()?)?),
        "list-style-type" => {
            let (t, s) = list_style_type(one()?)?;
            Decl::ListStyleType(t, s)
        }
        "list-style-position" => Decl::ListStylePosition(match kw()?.as_str() {
            "inside" => true,
            "outside" => false,
            _ => return None,
        }),
        "list-style-image" => return Some(()),
        "list-style" => {
            let mut ty = None;
            let mut inside = false;
            let mut nones = 0;
            for x in components(v) {
                match ident(x).as_deref() {
                    Some("inside") => inside = true,
                    Some("outside") => {}
                    Some("none") => nones += 1,
                    _ => {
                        if url_of(x, cx).is_some() {
                            continue;
                        }
                        ty = Some(list_style_type(x)?);
                    }
                }
            }
            if nones > 0 && ty.is_none() {
                ty = Some((ListStyleType::None, None));
            }
            let (t, s) = ty.unwrap_or((ListStyleType::Disc, None));
            out.push(Decl::ListStyleType(t, s));
            out.push(Decl::ListStylePosition(inside));
            return Some(());
        }
        "border-collapse" => Decl::BorderCollapse(match kw()?.as_str() {
            "collapse" => true,
            "separate" => false,
            _ => return None,
        }),
        "border-spacing" => {
            let c = components(v);
            let h = nonneg_len(c.first()?)?;
            let vv = if c.len() > 1 {
                nonneg_len(c[1])?
            } else {
                h.clone()
            };
            Decl::BorderSpacing(h, vv)
        }
        "caption-side" => Decl::CaptionSide(match kw()?.as_str() {
            "top" => false,
            "bottom" => true,
            _ => return None,
        }),
        "table-layout" => Decl::TableLayout(match kw()?.as_str() {
            "fixed" => true,
            "auto" => false,
            _ => return None,
        }),
        "cursor" => {
            let last = split_commas(v).last().copied()?;
            Decl::Cursor(match ident(last)?.as_str() {
                "pointer" | "hand" => Cursor::Pointer,
                "text" | "vertical-text" => Cursor::Text,
                "default" => Cursor::Default,
                _ => Cursor::Auto,
            })
        }
        "overflow-wrap" | "word-wrap" => Decl::BreakAnywhere(match kw()?.as_str() {
            "break-word" | "anywhere" => true,
            "normal" => false,
            _ => return None,
        }),
        "word-break" => Decl::BreakAnywhere(match kw()?.as_str() {
            "break-all" | "break-word" => true,
            "normal" | "keep-all" | "auto-phrase" => false,
            _ => return None,
        }),
        "letter-spacing" | "word-spacing" => {
            let c = one()?;
            let val = if ident(c).as_deref() == Some("normal") {
                None
            } else {
                Some(values::length(c, false)?)
            };
            if name == "letter-spacing" {
                Decl::LetterSpacing(val)
            } else {
                Decl::WordSpacing(val)
            }
        }
        "z-index" => Decl::ZIndex(if kw().as_deref() == Some("auto") {
            None
        } else {
            Some(values::integer(one()?)?)
        }),
        "opacity" => {
            let c = one()?;
            let o = match c.first()? {
                Tok::Percentage(p) => *p / 100.0,
                _ => values::number(c)?,
            };
            Decl::Opacity(o.clamp(0.0, 1.0))
        }
        "content" => Decl::Content(content(v)?),
        "flex-direction" | "-webkit-flex-direction" => Decl::FlexDirection(flex_direction(&kw()?)?),
        "flex-wrap" | "-webkit-flex-wrap" => Decl::FlexWrap(flex_wrap(&kw()?)?),
        "flex-flow" => {
            let mut dir = FlexDirection::Row;
            let mut wrap = FlexWrap::NoWrap;
            for x in components(v) {
                let k = ident(x)?;
                if let Some(d) = flex_direction(&k) {
                    dir = d;
                } else {
                    wrap = flex_wrap(&k)?;
                }
            }
            out.push(Decl::FlexDirection(dir));
            out.push(Decl::FlexWrap(wrap));
            return Some(());
        }
        "flex" | "-webkit-flex" => {
            flex(v, out)?;
            return Some(());
        }
        "flex-grow" | "-webkit-flex-grow" => Decl::FlexGrow(values::number(one()?)?.max(0.0)),
        "flex-shrink" | "-webkit-flex-shrink" => Decl::FlexShrink(values::number(one()?)?.max(0.0)),
        "flex-basis" | "-webkit-flex-basis" => Decl::FlexBasis(ssize(one()?)?),
        "order" => Decl::Order(values::integer(one()?)?),
        "justify-content" | "-webkit-justify-content" => {
            Decl::JustifyContent(justify(&align_ident(v)?)?)
        }
        "align-content" => Decl::AlignContent(justify(&align_ident(v)?)?),
        "align-items" | "-webkit-align-items" => Decl::AlignItems(align(&align_ident(v)?)?),
        "align-self" | "-webkit-align-self" => Decl::AlignSelf(align(&align_ident(v)?)?),
        "justify-items" | "justify-self" => return Some(()),
        "place-items" | "place-self" => {
            let c = components(v);
            let a = align(&ident(c.first()?)?)?;
            out.push(if name == "place-items" {
                Decl::AlignItems(a)
            } else {
                Decl::AlignSelf(a)
            });
            return Some(());
        }
        "place-content" => {
            let c = components(v);
            let a = justify(&ident(c.first()?)?)?;
            let j = if c.len() > 1 {
                justify(&ident(c[1])?)?
            } else {
                a
            };
            out.push(Decl::AlignContent(a));
            out.push(Decl::JustifyContent(j));
            return Some(());
        }
        "gap" | "grid-gap" => {
            let c = components(v);
            let norm = |x: &[Tok]| {
                if ident(x).as_deref() == Some("normal") {
                    Some(Len::px(0.0))
                } else {
                    nonneg_len(x)
                }
            };
            let r = norm(c.first()?)?;
            let col = if c.len() > 1 { norm(c[1])? } else { r.clone() };
            out.push(Decl::RowGap(r));
            out.push(Decl::ColumnGap(col));
            return Some(());
        }
        "row-gap" | "grid-row-gap" | "column-gap" | "grid-column-gap" => {
            let c = one()?;
            let l = if ident(c).as_deref() == Some("normal") {
                Len::px(0.0)
            } else {
                nonneg_len(c)?
            };
            if name.starts_with("row") || name == "grid-row-gap" {
                Decl::RowGap(l)
            } else {
                Decl::ColumnGap(l)
            }
        }
        "aspect-ratio" => {
            let c = components(v);
            let mut nums = Vec::new();
            for x in &c {
                match x.first() {
                    Some(Tok::Delim('/')) => {}
                    Some(Tok::Ident(k)) if k.eq_ignore_ascii_case("auto") => {}
                    _ => nums.push(values::number(x)?),
                }
            }
            Decl::AspectRatio(match nums.as_slice() {
                [] => None,
                [a] if *a > 0.0 => Some(*a),
                [a, b] if *a > 0.0 && *b > 0.0 => Some(*a / *b),
                _ => return None,
            })
        }
        "counter-reset" => Decl::CounterReset(counter(v, 0)?),
        "counter-increment" => Decl::CounterIncrement(counter(v, 1)?),
        // Known and accepted without effect (so @supports says yes and nothing breaks).
        "transform"
        | "transition"
        | "animation"
        | "box-shadow"
        | "text-shadow"
        | "outline"
        | "outline-color"
        | "outline-style"
        | "outline-width"
        | "outline-offset"
        | "filter"
        | "backdrop-filter"
        | "will-change"
        | "pointer-events"
        | "user-select"
        | "-webkit-user-select"
        | "resize"
        | "appearance"
        | "-webkit-appearance"
        | "-moz-appearance"
        | "text-overflow"
        | "direction"
        | "unicode-bidi"
        | "writing-mode"
        | "hyphens"
        | "tab-size"
        | "font-feature-settings"
        | "font-variant-numeric"
        | "font-variant-ligatures"
        | "font-kerning"
        | "font-stretch"
        | "font-optical-sizing"
        | "text-rendering"
        | "-webkit-font-smoothing"
        | "-moz-osx-font-smoothing"
        | "text-size-adjust"
        | "-webkit-text-size-adjust"
        | "-ms-text-size-adjust"
        | "quotes"
        | "clip"
        | "clip-path"
        | "mask"
        | "object-fit"
        | "object-position"
        | "scroll-behavior"
        | "overscroll-behavior"
        | "touch-action"
        | "contain"
        | "isolation"
        | "mix-blend-mode"
        | "columns"
        | "column-count"
        | "column-width"
        | "column-rule"
        | "grid-template-columns"
        | "grid-template-rows"
        | "grid-template-areas"
        | "grid-template"
        | "grid-area"
        | "grid-column"
        | "grid-row"
        | "grid-auto-flow"
        | "grid-auto-rows"
        | "grid-auto-columns"
        | "grid-column-start"
        | "grid-column-end"
        | "grid-row-start"
        | "grid-row-end"
        | "caret-color"
        | "accent-color"
        | "color-scheme"
        | "forced-color-adjust"
        | "print-color-adjust"
        | "-webkit-print-color-adjust"
        | "speak"
        | "content-visibility"
        | "container"
        | "container-type"
        | "container-name"
        | "scrollbar-width"
        | "scrollbar-color"
        | "scrollbar-gutter"
        | "-webkit-tap-highlight-color"
        | "zoom"
        | "empty-cells"
        | "image-rendering"
        | "font-display"
        | "src"
        | "unicode-range"
        | "orphans"
        | "widows"
        | "page-break-inside"
        | "page-break-before"
        | "page-break-after"
        | "break-inside"
        | "break-before"
        | "break-after"
        | "font-synthesis"
        | "hanging-punctuation"
        | "line-break"
        | "text-justify"
        | "text-align-last"
        | "-webkit-line-clamp"
        | "-webkit-box-orient"
        | "line-clamp"
        | "translate"
        | "rotate"
        | "scale"
        | "backface-visibility"
        | "perspective"
        | "transform-origin"
        | "transform-style"
        | "transition-property"
        | "transition-duration"
        | "transition-timing-function"
        | "transition-delay"
        | "animation-name"
        | "animation-duration"
        | "animation-timing-function"
        | "animation-delay"
        | "animation-iteration-count"
        | "animation-direction"
        | "animation-fill-mode"
        | "animation-play-state"
        | "inset-inline"
        | "inset-block"
        | "inset-inline-start"
        | "inset-inline-end"
        | "inset-block-start"
        | "inset-block-end"
        | "text-emphasis"
        | "shape-outside"
        | "shape-margin"
        | "vertical-align-last"
        | "-webkit-mask"
        | "-webkit-mask-image"
        | "mask-image"
        | "fill"
        | "stroke"
        | "stroke-width"
        | "box-decoration-break"
        | "-webkit-box-decoration-break" => {
            return Some(());
        }
        _ => return None,
    };
    out.push(d);
    Some(())
}

/// Parse a declaration block (a `style` attribute or a rule body) into longhands, split into
/// normal and !important. CSS Syntax §5.4.5 "consume a list of declarations".
pub fn parse_block(toks: &[Tok], cx: &ParseCx, normal: &mut Vec<Decl>, important: &mut Vec<Decl>) {
    let mut i = 0;
    while i < toks.len() {
        match &toks[i] {
            Tok::Whitespace | Tok::Semicolon => {
                i += 1;
            }
            Tok::Ident(name) => {
                // Up to the next top-level semicolon.
                let mut j = i + 1;
                while j < toks.len() && !matches!(toks[j], Tok::Semicolon) {
                    j = values::component_end(toks, j).max(j + 1);
                }
                let decl = &toks[i + 1..j.min(toks.len())];
                let k = values::skip_ws(decl, 0);
                if matches!(decl.get(k), Some(Tok::Colon)) {
                    let mut val = trim(&decl[k + 1..]);
                    let mut imp = false;
                    // "! important" at the end.
                    if val.len() >= 2
                        && let Tok::Ident(w) = &val[val.len() - 1]
                        && w.eq_ignore_ascii_case("important")
                    {
                        let mut b = val.len() - 1;
                        while b > 0 && matches!(val[b - 1], Tok::Whitespace) {
                            b -= 1;
                        }
                        if b > 0 && matches!(val[b - 1], Tok::Delim('!')) {
                            imp = true;
                            val = trim(&val[..b - 1]);
                        }
                    }
                    let target = if imp { &mut *important } else { &mut *normal };
                    parse_declaration(name, val, cx, target);
                }
                i = j + 1;
            }
            _ => {
                // Not a declaration (a nested rule or junk): skip to the next semicolon at
                // this level, or past a {} block.
                let mut j = i;
                while j < toks.len() {
                    if matches!(toks[j], Tok::Semicolon) {
                        break;
                    }
                    let opened_block = matches!(toks[j], Tok::Open('{'));
                    j = values::component_end(toks, j).max(j + 1);
                    if opened_block {
                        break;
                    }
                }
                i = j + 1;
            }
        }
    }
}
