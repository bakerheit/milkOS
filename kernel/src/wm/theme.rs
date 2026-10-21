//! Design tokens from `docs/ui-concepts/SPEC.md` (milkOS 1.0). Every number the shell draws with
//! lives here, named as in the spec.

use crate::gfx::{Color, rgb};

/// The screen size (the display mode, Settings > Display & Lock): 1024x768 at boot.
static SCREEN: [core::sync::atomic::AtomicI32; 2] = [
    core::sync::atomic::AtomicI32::new(1024),
    core::sync::atomic::AtomicI32::new(768),
];

pub fn screen_w() -> i32 {
    SCREEN[0].load(core::sync::atomic::Ordering::Relaxed)
}

pub fn screen_h() -> i32 {
    SCREEN[1].load(core::sync::atomic::Ordering::Relaxed)
}

/// The desktop changed the display mode (wm thread).
pub fn set_screen(w: i32, h: i32) {
    SCREEN[0].store(w, core::sync::atomic::Ordering::Relaxed);
    SCREEN[1].store(h, core::sync::atomic::Ordering::Relaxed);
}

// ---- colours ------------------------------------------------------------------------------
pub const MILK: Color = rgb(0xFF, 0xFD, 0xF8);
pub const CREAM: Color = rgb(0xFB, 0xF6, 0xEC);
pub const OAT: Color = rgb(0xEF, 0xE6, 0xD4);
pub const SAND: Color = rgb(0xE2, 0xD5, 0xBD);
pub const LATTE: Color = rgb(0xC4, 0xAE, 0x8F);
pub const COCOA: Color = rgb(0x6B, 0x58, 0x46);
pub const ESPRESSO: Color = rgb(0x3A, 0x2E, 0x28);
pub const STRAW: Color = rgb(0xE5, 0x8C, 0x97);
pub const HONEY: Color = rgb(0xEB, 0xB6, 0x50);
pub const MATCHA: Color = rgb(0x9C, 0xBF, 0x9B);
pub const BLUE: Color = rgb(0x7F, 0x93, 0xC9);
#[allow(dead_code)] // wallpaper only, and the current wallpaper's gradient ends near it
pub const LAV: Color = rgb(0xB9, 0xA9, 0xD6);
pub const WHITE: Color = rgb(0xFF, 0xFF, 0xFF);
/// Terminal title bar and border.
pub const TERM_BAR: Color = rgb(0x4A, 0x3C, 0x34);

// ---- fixed colours (the same in both themes) ----------------------------------------------
// These sit one LSB away from the tokens they resemble, so the theme map below leaves them
// alone: the Terminal is espresso in both themes, shadows and dims are always dark, glyphs
// on coloured tiles are always ink.
/// Terminal body.
pub const TERM_BG: Color = rgb(0x3A, 0x2E, 0x29);
/// Terminal text and the light glyph on the Terminal tile.
pub const TERM_FG: Color = rgb(0xFB, 0xF6, 0xED);
/// Shadows, dims, glyphs on honey / straw / blue tiles.
pub const INK: Color = rgb(0x3A, 0x2E, 0x27);
/// Dimmed Terminal text (program output, the unfocused cursor): latte one LSB off, so it
/// stays 6.1:1 on the Terminal body in both themes (the dark latte would be 3.08:1).
pub const TERM_DIM: Color = rgb(0xC4, 0xAE, 0x8E);

// ---- themes ---------------------------------------------------------------------------------

static DARK: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Is "Chocolate milk" (the dark theme) on?
pub fn dark() -> bool {
    DARK.load(core::sync::atomic::Ordering::Relaxed)
}

pub fn set_dark(on: bool) {
    DARK.store(on, core::sync::atomic::Ordering::Relaxed);
}

/// The token table: what each neutral token is in the current theme. The light theme
/// ("Whole milk") is the SPEC. Accents (straw, honey, matcha, blue, lav) stay as they are.
/// The painter runs every colour through this, so everything drawn with tokens follows the
/// theme without knowing about it.
#[inline]
pub fn tc(c: Color) -> Color {
    if !dark() {
        return c;
    }
    match c {
        MILK => rgb(0x2E, 0x25, 0x20),     // surfaces, window bodies
        CREAM => rgb(0x38, 0x2D, 0x27),    // title bars
        OAT => rgb(0x42, 0x35, 0x2D),      // sidebars
        SAND => rgb(0x55, 0x46, 0x3C),     // hairlines
        LATTE => rgb(0x8C, 0x77, 0x64),    // disabled text, inactive lights
        COCOA => rgb(0xBE, 0xA8, 0x90),    // secondary text
        ESPRESSO => rgb(0xF4, 0xEB, 0xE0), // primary text
        WHITE => rgb(0x7A, 0x66, 0x58),    // glass edges, hover lightening
        _ => c,
    }
}

/// Alpha as a 0..=255 byte from a percentage-ish fraction written as thousandths.
pub const fn a(thousandths: u32) -> u8 {
    ((thousandths * 255 + 500) / 1000) as u8
}

/// Selection rows: straw at 0x44.
pub const SEL_ROW_A: u8 = 0x44;
/// Sidebar selection: straw at 0x55.
pub const SEL_SIDEBAR_A: u8 = 0x55;
/// Folder tiles: honey at 0x55.
pub const FOLDER_A: u8 = 0x55;

// ---- radii ---------------------------------------------------------------------------------
pub const R_WINDOW: i32 = 12;
pub const R_DOCK: i32 = 20;
pub const R_LAUNCHER: i32 = 18;
pub const R_CARD: i32 = 20;
pub const R_TOAST: i32 = 16;
pub const R_BUTTON: i32 = 8;
pub const R_ROW: i32 = 12;

// ---- glass ----------------------------------------------------------------------------------
pub const BLUR_RADIUS: usize = 24;
pub const BLUR_PASSES: usize = 3;
pub const GLASS_A: u8 = a(550);
pub const GLASS_STRONG_A: u8 = a(700);
pub const GLASS_BORDER_A: u8 = a(750);

// ---- shadows (grow, dy, alpha) -------------------------------------------------------------
pub const SHADOW_WINDOW: [(i32, i32, u8); 3] = [(10, 12, a(50)), (5, 8, a(70)), (0, 4, a(120))];
pub const SHADOW_FLOAT: [(i32, i32, u8); 2] = [(6, 6, a(50)), (3, 4, a(70))];

// ---- menu bar -------------------------------------------------------------------------------
pub const MENU_H: i32 = 28;
pub const MENU_PAD_X: i32 = 12;
pub const MENU_TEXT_Y: i32 = 6;

// ---- dock -----------------------------------------------------------------------------------
pub const DOCK_PAD: i32 = 8;
/// Dock icon size: 48 by SPEC; Settings offers 40 / 48 / 56. The dock is that plus padding.
static DOCK_ICON_PX: core::sync::atomic::AtomicI32 = core::sync::atomic::AtomicI32::new(48);

pub fn dock_icon() -> i32 {
    DOCK_ICON_PX.load(core::sync::atomic::Ordering::Relaxed)
}

pub fn set_dock_icon(px: i32) {
    DOCK_ICON_PX.store(px.clamp(32, 64), core::sync::atomic::Ordering::Relaxed);
}

/// Dock height (64 at the SPEC's 48px icons).
pub fn dock_h() -> i32 {
    dock_icon() + 2 * DOCK_PAD
}
pub const DOCK_GAP: i32 = 12;
pub const DOCK_BOTTOM: i32 = 12;
pub const DOCK_DOT: i32 = 4;
pub const DOCK_DOT_GAP: i32 = 4;

// ---- windows --------------------------------------------------------------------------------
pub const TITLE_H: i32 = 32;
pub const LIGHT_D: i32 = 12;
pub const LIGHT_GAP: i32 = 8;
pub const LIGHT_X: i32 = 14;
pub const LIGHT_Y: i32 = 10;
pub const WIN_MIN_W: i32 = 240;
pub const WIN_MIN_H: i32 = 160;
pub const BODY_PAD: i32 = 16;
pub const SIDEBAR_W: i32 = 160;
pub const STATUS_H: i32 = 24;

// ---- launcher -------------------------------------------------------------------------------
pub const LAUNCH_W: i32 = 640;
pub const LAUNCH_Y: i32 = 140;
pub const LAUNCH_INPUT_H: i32 = 56;
pub const LAUNCH_ROW_H: i32 = 48;
pub const LAUNCH_GAP: i32 = 12;
pub const LAUNCH_MAX_ROWS: usize = 6;

/// Skim's result rows: SPEC's 6 at 768 high, more on taller screens.
pub fn launch_rows() -> usize {
    match screen_h() {
        0..800 => LAUNCH_MAX_ROWS,
        800..900 => 8,
        _ => 9,
    }
}
pub const LAUNCH_DIM_A: u8 = a(250);

// ---- toast ----------------------------------------------------------------------------------
pub const TOAST_W: i32 = 264;
pub const TOAST_H: i32 = 64;
pub const TOAST_RIGHT: i32 = 12;
pub const TOAST_TOP: i32 = 40;

// ---- control centre -------------------------------------------------------------------------
pub const CC_W: i32 = 288;
pub const CC_H: i32 = 292;
pub const CC_RIGHT: i32 = 12;
pub const CC_TOP: i32 = 36;
pub const CC_TILE_H: i32 = 56;
pub const CC_SLIDER_H: i32 = 8;

// ---- home cards -----------------------------------------------------------------------------
pub const CARD_PAD: i32 = 16;
pub const CARD_GAP: i32 = 16;
pub const CARD_MARGIN: i32 = 48;

// ---- spaces ---------------------------------------------------------------------------------
pub const SPACES_DIM_A: u8 = a(350);
pub const SPACES_STRIP_Y: i32 = 40;
pub const THUMB_W: i32 = 128;
pub const THUMB_H: i32 = 88;
pub const THUMB_GAP: i32 = 12;
pub const THUMB_R: i32 = 10;
pub const SEL_BORDER: i32 = 4;
