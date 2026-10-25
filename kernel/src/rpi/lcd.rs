//! The 3.5-inch SPI display and its touch chip (docs/raspberry-pi.md, "3.5-inch SPI display"):
//! the kernel's half. What can be decided from bytes is `rpikit::lcd` and `rpikit::touch`
//! (command streams, pixel packing, scaling, calibration, filtering; host tested against a model
//! of both chips); this file wires them to SPI0 and the GPIO header, to the boot console and the
//! crash screen, to the display layer and to the input layer.
//!
//! Off unless the command line says `milk.lcd=ili9486` (or another controller of `rpikit::lcd`, or
//! `probe`: show each candidate set-up in turn, see [`probe`]): nothing on the header is touched
//! otherwise. Options, all of them in cmdline.txt (docs/raspberry-pi.md):
//!
//! * `milk.lcd=` controller, `milk.lcd_rot=0|90|180|270` (default 90: landscape 480x320),
//!   `milk.lcd_bgr=0|1` (default 1), `milk.lcd_inv=0|1`, `milk.lcd_madctl=0x..` (the raw memory
//!   access control byte, for a glass that is mirrored differently);
//! * pins: `milk.lcd_dc=` (24), `milk.lcd_rst=` (25), `milk.lcd_bl=` (none: the backlight is
//!   assumed wired on), `milk.touch_irq=` (17, `-1` to poll only), all GPIO numbers 2..=27; the
//!   display is on chip select 0 and the touch chip on chip select 1;
//! * `milk.lcd_hz=` (16000000, up to 32000000: the bit clock for the display), `milk.lcd_spi_mode=`
//!   (0..3, default 0: the SPI clock polarity and phase);
//! * `milk.touch=0|1`, `milk.touch_cal=a,b,c,d,e,f` (or `ask` for the four-cross helper);
//! * `milk.lcd_view=fit|pan` (the desktop on the small screen: all of it scaled down, or 1:1
//!   with the view following a finger held at the edge), `milk.hdmi=off` (do not use the
//!   firmware's HDMI framebuffer: the desktop is drawn in RAM and the panel is the screen).
//!
//! The panel can show three things: the boot log (a text console in its own 480x320 shadow,
//! wrapping instead of scrolling because every line costs milliseconds on the wire), the crash
//! screen (the end of the kernel log, drawn with interrupts off and nothing allocated), and the
//! desktop (a scaled or cropped view of whatever framebuffer the desktop draws on, sent in the
//! rectangles that changed). The SPI write routines never allocate.
//!
//! Nothing here may hang or panic when the hardware is absent, wrong or silent: SPI0 is probed
//! first (qemu and boards without the controller read 0), a panel cannot be probed (the kits
//! cannot be read back) so a driver that gets no answer simply sends into the void, every wait is
//! bounded, and three failed transfers in a row switch the panel off.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

use rpikit::cmdline;
use rpikit::lcd::{
    self as pk, Bus, Config, Controller, Dirty, Pixels, Rect, Rotation, View, ViewSource,
};
use rpikit::mailbox::clock;
use rpikit::touch::{self, Cal, Event, Tracker};

use super::{Timeout, mailbox, spi};
use crate::fbcon::Console;
use crate::gfx::Surface;
use crate::input::{self, MouseButton};
use crate::sync::SpinLock;
use crate::{display, kprintln, sink};

/// What [`STATE`] says.
const OFF: u8 = 0;
const READY: u8 = 1;
const FAILED: u8 = 2;

/// What the panel shows ([`MODE`]).
const TEXT: u8 = 0;
const DESKTOP: u8 = 1;

static STATE: AtomicU8 = AtomicU8::new(OFF);
static MODE: AtomicU8 = AtomicU8::new(TEXT);
/// A crash screen has been drawn: nothing else goes to the panel any more.
static CRASHED: AtomicBool = AtomicBool::new(false);
/// SPI0 is in use (the display and the touch chip share it).
static BUS: AtomicBool = AtomicBool::new(false);
/// Interrupts are on (set by kmain): before that, nothing can interrupt a transfer and masked
/// interrupts are the normal state; after it, masked interrupts mean an interrupt handler or a
/// critical section, where the print path must not spend milliseconds.
static IRQS_LIVE: AtomicBool = AtomicBool::new(false);
/// The calibration helper is running: the desktop waits for it.
static CAL_BUSY: AtomicBool = AtomicBool::new(false);
static FAILS: AtomicU8 = AtomicU8::new(0);
static FIRST_FRAME: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy)]
struct Settings {
    cfg: Config,
    dc: u8,
    rst: u8,
    irq: Option<u8>,
    cdiv: u32,
    /// SPI mode of the display (the touch chip is always mode 0).
    mode: u8,
    touch_cdiv: u32,
    touch: bool,
    cal: Cal,
    cal_ask: bool,
    pan: bool,
    hdmi_off: bool,
}

struct SettingsCell(UnsafeCell<Option<Settings>>);

// SAFETY: written once by `init` before STATE leaves OFF (Release), read-only afterwards.
unsafe impl Sync for SettingsCell {}

static SETTINGS: SettingsCell = SettingsCell(UnsafeCell::new(None));

fn settings() -> Option<Settings> {
    if STATE.load(Ordering::Acquire) != READY {
        return None;
    }
    // SAFETY: see SettingsCell.
    unsafe { *SETTINGS.0.get() }
}

/// What has to be sent, and how the picture is laid out (taken together under one lock).
struct Inner {
    dirty: Dirty,
    view: View,
}

static INNER: SpinLock<Option<Inner>> = SpinLock::new(None);

/// The panel-sized RAM picture the text consoles draw on: XRGB, `width` x `height` of the
/// rotation, in the first `PIXELS` words. Static, so the crash screen needs no allocator.
struct Shadow(UnsafeCell<[u32; pk::PIXELS]>);

// SAFETY: one console draws on it at a time (sink.rs' flag, or the crash path, which stops the
// others), and the driver only reads it.
unsafe impl Sync for Shadow {}

static SHADOW: Shadow = Shadow(UnsafeCell::new([0; pk::PIXELS]));

fn shadow_surface(cfg: &Config) -> Surface<'static> {
    // SAFETY: the static above, 4-byte aligned, `width * height <= PIXELS` words.
    unsafe {
        Surface::from_raw(
            SHADOW.0.get() as *mut u32,
            cfg.width(),
            cfg.height(),
            cfg.width(),
        )
    }
}

fn now_us() -> u64 {
    let (c, f): (u64, u64);
    // SAFETY: register reads.
    unsafe {
        core::arch::asm!("isb", "mrs {}, cntvct_el0", out(reg) c, options(nomem, nostack));
        core::arch::asm!("mrs {}, cntfrq_el0", out(reg) f, options(nomem, nostack));
    }
    let f = if f == 0 { 54_000_000 } else { f };
    c / (f / 1_000_000).max(1)
}

// ---- options ------------------------------------------------------------------------------------

fn option(name: &str) -> Option<&'static [u8]> {
    crate::fwcfg::option(name)
}

fn flag(name: &str, default: bool) -> bool {
    match option(name) {
        Some(b"0" | b"off" | b"no" | b"false") => false,
        Some(b"1" | b"on" | b"yes" | b"true") => true,
        _ => default,
    }
}

fn pin(name: &str, default: u8) -> u8 {
    match option(name).and_then(cmdline::parse_num) {
        None => default,
        Some(n) => match spi::header_pin(n) {
            Some(p) => p,
            None => {
                kprintln!("lcd: milk.{name}={n} is not a header GPIO (2..27), using {default}");
                default
            }
        },
    }
}

fn parse_settings(controller: Controller, core_hz: u32) -> Settings {
    let mut cfg = Config::new(controller);
    if let Some(v) = option("lcd_rot") {
        match Rotation::parse(v) {
            Some(r) => cfg.rotation = r,
            None => kprintln!("lcd: milk.lcd_rot must be 0, 90, 180 or 270, using 90"),
        }
    }
    cfg.bgr = flag("lcd_bgr", true);
    if option("lcd_inv").is_some() {
        cfg.invert = Some(flag("lcd_inv", false));
    }
    cfg.madctl = option("lcd_madctl")
        .and_then(cmdline::parse_num)
        .filter(|&v| (0..=255).contains(&v))
        .map(|v| v as u8);
    let hz = option("lcd_hz")
        .and_then(cmdline::parse_num)
        .filter(|&v| v > 0)
        .map_or(16_000_000, |v| (v as u64).min(32_000_000) as u32);
    let irq = match option("touch_irq") {
        Some(b"-1" | b"none" | b"off") => None,
        _ => Some(pin("touch_irq", 17)),
    };
    let cal_opt = option("touch_cal");
    let cal_ask = cal_opt == Some(b"ask");
    let cal = cal_opt
        .filter(|_| !cal_ask)
        .and_then(|v| {
            let c = Cal::parse(v);
            if c.is_none() {
                kprintln!(
                    "lcd: milk.touch_cal needs six numbers a,b,c,d,e,f (or ask), using the default"
                );
            }
            c
        })
        .unwrap_or_else(|| Cal::default_for(cfg.rotation));
    let mode = match option("lcd_spi_mode") {
        None => 0,
        Some(v) => pk::parse_spi_mode(v).unwrap_or_else(|| {
            kprintln!("lcd: milk.lcd_spi_mode must be 0, 1, 2 or 3, using 0");
            0
        }),
    };
    Settings {
        cfg,
        dc: pin("lcd_dc", 24),
        rst: pin("lcd_rst", 25),
        irq,
        cdiv: spi::divider_for(core_hz, hz),
        mode,
        touch_cdiv: spi::divider_for(core_hz, 1_000_000),
        touch: flag("touch", true),
        cal,
        cal_ask,
        pan: option("lcd_view") == Some(&b"pan"[..]),
        hdmi_off: option("hdmi") == Some(&b"off"[..]),
    }
}

// ---- the bus ------------------------------------------------------------------------------------

struct BusGuard;

impl Drop for BusGuard {
    fn drop(&mut self) {
        BUS.store(false, Ordering::Release);
    }
}

/// Take SPI0, waiting up to `wait_ms` (0: only if it is free right now).
fn lock_bus(wait_ms: u64) -> Option<BusGuard> {
    let t = Timeout::ms(wait_ms);
    loop {
        if BUS
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            return Some(BusGuard);
        }
        if t.expired() {
            return None;
        }
        core::hint::spin_loop();
    }
}

/// The display on SPI0 chip select 0, D/C and reset on GPIO pins.
struct HwBus {
    dc: u8,
    rst: u8,
    cdiv: u32,
    mode: u8,
}

impl Bus for HwBus {
    fn write(&mut self, data: bool, bytes: &[u8]) -> bool {
        spi::write(self.dc, data);
        spi::transfer_mode(0, self.cdiv, self.mode, bytes, None)
    }
    fn reset_pin(&mut self, high: bool) {
        spi::write(self.rst, high);
    }
    fn delay_ms(&mut self, ms: u32) {
        let t = Timeout::ms(ms as u64);
        while !t.expired() {
            core::hint::spin_loop();
        }
    }
}

/// The touch chip on chip select 1.
struct TouchHw {
    cdiv: u32,
}

impl touch::TouchBus for TouchHw {
    fn transfer(&mut self, tx: &[u8], rx: &mut [u8]) -> bool {
        spi::transfer(1, self.cdiv, tx, Some(rx))
    }
}

// ---- bring-up -----------------------------------------------------------------------------------

/// The core clock SPI0 is divided from when the firmware does not say (its mailbox failed, or answered
/// nonsense): the Pi 4's documented default `core_freq` of 500 MHz (the Pi 3 and older boards run
/// 250..400 MHz; the Pi 4 clock follows its load between `core_freq_min` 200 and `core_freq` 500).
/// The dividers are computed from the highest rate the clock reaches, so a clock that is really lower
/// makes the bus slower than asked, never faster; with an overclocked `core_freq` (up to 550) it can be at most
/// 10% faster. The wrong guess therefore costs speed, not correctness, and the transfer time-outs
/// allow six times the time the wire needs.
const FALLBACK_CORE_HZ: u32 = 500_000_000;

/// The core clock for the dividers, and a few words on where it came from (for the log).
fn core_clock() -> (u32, &'static str) {
    // The divider comes from the highest rate the core clock reaches (it follows the load: a
    // divider for the idle rate would make the bus faster than asked when the clock rises).
    match mailbox::max_clock_rate(clock::CORE) {
        Ok(hz) if hz >= 100_000_000 => (hz, "from the firmware"),
        other => {
            kprintln!(
                "lcd: the firmware did not give the core clock ({other:?}), assuming the Pi 4's default 500 MHz"
            );
            (FALLBACK_CORE_HZ, "fallback, no firmware answer")
        }
    }
}

/// Set up SPI0, the pins and the panel if the command line asks for one. Call once after
/// `mem::init` (the firmware mailbox needs memory) and before the display layer starts. Logs
/// what it does; failure leaves the panel off and the boot going.
pub fn init() {
    let Some(kind) = option("lcd") else {
        return;
    };
    if matches!(kind, b"" | b"off" | b"none" | b"0") {
        return;
    }
    let probing = kind.eq_ignore_ascii_case(b"probe");
    let Some(controller) = Controller::parse(kind).or(probing.then_some(Controller::Ili9486)) else {
        STATE.store(FAILED, Ordering::Release);
        kprintln!(
            "lcd: milk.lcd must be ili9486, ili9486p, ili9486p18, ili9488, st7796, ili9481, r61581, hx8357, ili9341 or st7789 (or probe, or off), no display driver"
        );
        return;
    };
    let (core_hz, core_src) = core_clock();
    spi::set_core_hz(core_hz);
    let s = parse_settings(controller, core_hz);
    if probing {
        probe(&s, core_hz, core_src);
        return;
    }
    let touch_desc = match (s.touch, s.irq) {
        (false, _) => alloc::string::String::from("off"),
        (true, Some(p)) => alloc::format!("XPT2046 on CE1, pen IRQ GPIO{p}"),
        (true, None) => alloc::string::String::from("XPT2046 on CE1, polled"),
    };
    kprintln!(
        "lcd: {} {}x{} rot {}{}, SPI0 CE0 {} kHz (core {} MHz {} / {}, mode {}), DC=GPIO{} RST=GPIO{}, touch {}",
        controller.name(),
        s.cfg.width(),
        s.cfg.height(),
        s.cfg.rotation.degrees(),
        if s.cfg.bgr { " bgr" } else { "" },
        core_hz / s.cdiv / 1000,
        core_hz / 1_000_000,
        core_src,
        s.cdiv,
        s.mode,
        s.dc,
        s.rst,
        touch_desc
    );
    if !connect(&s) {
        STATE.store(FAILED, Ordering::Release);
        return;
    }
    let mut bus = HwBus {
        dc: s.dc,
        rst: s.rst,
        cdiv: s.cdiv,
        mode: s.mode,
    };
    if !pk::init(&s.cfg, &mut bus) {
        STATE.store(FAILED, Ordering::Release);
        kprintln!("lcd: SPI transfers to the panel time out: no display on the header");
        return;
    }
    // SAFETY: nobody reads the cell before STATE says READY.
    unsafe { *SETTINGS.0.get() = Some(s) };
    *INNER.lock() = Some(Inner {
        dirty: Dirty::new(),
        view: View::scaled(s.cfg.width(), s.cfg.height(), s.cfg.width(), s.cfg.height()),
    });
    STATE.store(READY, Ordering::Release);
    kprintln!(
        "lcd: panel initialised (write only: whether it shows anything is for your eyes to say)"
    );
}

/// The pins and SPI0 for the display. False (and a log line) if the controller is not there.
fn connect(s: &Settings) -> bool {
    spi::connect_pins();
    spi::set_function(s.dc, spi::Func::Output);
    spi::set_function(s.rst, spi::Func::Output);
    spi::write(s.rst, true);
    spi::write(s.dc, false);
    if let Some(p) = s.irq {
        spi::set_function(p, spi::Func::Input);
        spi::set_pull(p, spi::Pull::Up);
    }
    if let Some(bl) = option("lcd_bl")
        .and_then(cmdline::parse_num)
        .and_then(spi::header_pin)
    {
        spi::set_function(bl, spi::Func::Output);
        spi::write(bl, true);
    }
    if !spi::present() {
        kprintln!("lcd: SPI0 does not answer (no such controller here?): no display on the header");
        return false;
    }
    spi::reset();
    true
}

/// Busy-wait until `ms` milliseconds have passed since `t0` (a [`now_us`] reading).
fn wait_since(t0: u64, ms: u32) {
    while now_us().saturating_sub(t0) < u64::from(ms) * 1000 {
        core::hint::spin_loop();
    }
}

/// Why a probe set-up was given up.
#[derive(Clone, Copy)]
enum Skip {
    Init,
    NumberCard,
    Flash,
    TextCard,
}

impl Skip {
    fn name(self) -> &'static str {
        match self {
            Skip::Init => "reset and init",
            Skip::NumberCard => "number card",
            Skip::Flash => "colour flash",
            Skip::TextCard => "text card",
        }
    }
}

/// `milk.lcd=probe`, for a display whose controller is unknown (the kits cannot be read back): run
/// through the matrix of candidate set-ups of `rpikit::lcdprobe` (controller, SPI mode, clock, D/C and
/// reset pins). Each set-up is reset and initialised again, then shows, in this order: its number
/// (huge, white on black), four full-screen colour flashes (red, green, blue, white; a slow clock sends
/// only the rows that fit in the time), and a text card (name, options, `SETUP n OF N`, bars). Then the
/// next. The list is shown `milk.lcd_probe_rounds=` times (default 1); then the boot goes on with no
/// panel in use (the person puts the options of the set-up they could see into cmdline.txt). The text
/// card stays `milk.lcd_probe_ms=` ms (default 1000), the number card as long, a flash two fifths of it;
/// `milk.lcd_probe_from=n` starts at set-up n; `milk.lcd_probe_pins=0` leaves out the backlight pins
/// and the round of other D/C and reset pairs.
///
/// While probing, the GPIOs `lcdprobe::BACKLIGHT_PINS` (18 and 27) are driven high as a backlight switch,
/// unless `milk.lcd_bl` names the pin or the pin is the set-up's D/C or reset (they are outputs into a
/// 3.3 V logic input, nothing more); they stay high when the probe is over.
///
/// Every set-up logs a `lcd: probe n/N name ...` line when it starts (with the SPI mode, the divider and
/// the clock it makes of the core clock, and where the core clock came from) and another when it ends
/// (how long it took, how fast the text card went over the wire), so the SD-card log says exactly what
/// ran. Bounded: every wait is, and a set-up whose transfers fail is skipped.
fn probe(base: &Settings, core_hz: u32, core_src: &'static str) {
    use rpikit::lcdprobe as lp;
    let opts = lp::ProbeOptions::parse(option);
    let count = opts.count();
    let user_bl = option("lcd_bl")
        .and_then(cmdline::parse_num)
        .and_then(spi::header_pin);
    kprintln!(
        "lcd: probe: {} set-ups from {} of {}, {} round(s), text card {} ms, number card {} ms, colour flashes {} ms, pin sweeps {} (D/C GPIO{} reset GPIO{} unless a set-up names its own; SPI core clock {} MHz, {})",
        count - opts.from + 1,
        opts.from,
        count,
        opts.rounds,
        opts.hold_ms,
        lp::number_ms(opts.hold_ms),
        lp::flash_ms(opts.hold_ms),
        if opts.pins { "on" } else { "off" },
        base.dc,
        base.rst,
        core_hz / 1_000_000,
        core_src
    );
    spi::connect_pins();
    if let Some(p) = base.irq {
        spi::set_function(p, spi::Func::Input);
        spi::set_pull(p, spi::Pull::Up);
    }
    if !spi::present() {
        kprintln!("lcd: SPI0 does not answer (no such controller here?): no display on the header");
        return;
    }
    spi::reset();
    if let Some(bl) = user_bl {
        spi::set_function(bl, spi::Func::Output);
        spi::write(bl, true);
    }
    let base_pins = (base.dc, base.rst);
    let mut prev: Option<(u8, u8)> = None;
    let t_all = now_us();
    let (mut shown, mut skipped) = (0u32, 0u32);
    for round in 1..=opts.rounds {
        for i in opts.from - 1..count {
            let setup = &lp::SETUPS[i];
            let n = i + 1;
            let t_setup = now_us();
            let (dc, rst) = setup.pins_or(base_pins);
            let cdiv = spi::divider_for(core_hz, setup.hz);
            let desc = lp::describe(setup, base_pins);
            // The pins: the previous set-up's D/C and reset go back to inputs, this one's become outputs
            // (D/C low, reset high), and the backlight pins that are free are driven high.
            if let Some((pd, pr)) = prev
                && (pd, pr) != (dc, rst)
            {
                for p in [pd, pr] {
                    if p != dc && p != rst {
                        spi::set_function(p, spi::Func::Input);
                    }
                }
            }
            prev = Some((dc, rst));
            spi::set_function(dc, spi::Func::Output);
            spi::set_function(rst, spi::Func::Output);
            spi::write(rst, true);
            spi::write(dc, false);
            let mut bl_text = alloc::string::String::new();
            if opts.pins && user_bl.is_none() {
                for bl in lp::BACKLIGHT_PINS {
                    if bl != dc && bl != rst && Some(bl) != base.irq {
                        spi::set_function(bl, spi::Func::Output);
                        spi::write(bl, true);
                        let _ = core::fmt::Write::write_fmt(&mut bl_text, format_args!(" GPIO{bl}"));
                    }
                }
            }
            kprintln!(
                "lcd: probe {n}/{count} {}: start (round {round}/{}): {}; SPI mode {} divider {cdiv} = {} kHz of core {} MHz ({core_src}); backlight high:{}",
                desc.as_str(),
                opts.rounds,
                lp::options(setup, base_pins).as_str(),
                setup.mode,
                core_hz / cdiv / 1000,
                core_hz / 1_000_000,
                if bl_text.is_empty() { " none by the probe" } else { bl_text.as_str() }
            );
            let mut bus = HwBus {
                dc,
                rst,
                cdiv,
                mode: setup.mode,
            };
            let cfg = setup.config();
            let (w, h) = (cfg.width(), cfg.height());
            spi::reset();
            let mut text_ms = 0u64;
            let result = (|| {
                if !pk::init(&cfg, &mut bus) {
                    return Err(Skip::Init);
                }
                // 1. The number card (rows from the top while the time lasts).
                let t = now_us();
                let budget = lp::number_ms(opts.hold_ms);
                let mut card = lp::NumberCard::new(n, count, setup, w, h);
                let mut stop = || now_us().saturating_sub(t) >= u64::from(budget) * 1000;
                if lp::draw_banded(&cfg, &mut bus, &mut card, &mut stop).is_none() {
                    return Err(Skip::NumberCard);
                }
                wait_since(t, budget);
                // 2. The colour flashes.
                let budget = lp::flash_ms(opts.hold_ms);
                for c in lp::FLASHES {
                    let t = now_us();
                    let mut stop = || now_us().saturating_sub(t) >= u64::from(budget) * 1000;
                    if lp::draw_banded(&cfg, &mut bus, &mut pk::Solid(c), &mut stop).is_none() {
                        return Err(Skip::Flash);
                    }
                    wait_since(t, budget);
                }
                // 3. The text card, all of it, then held.
                let mut card = lp::ProbeCard::new(n, count, setup, base_pins, w, h);
                let t = now_us();
                if !pk::draw_rect(&cfg, &mut bus, Rect::new(0, 0, w, h), &mut card) {
                    return Err(Skip::TextCard);
                }
                text_ms = (now_us() - t) / 1000;
                wait_since(now_us(), opts.hold_ms);
                Ok(())
            })();
            let took = (now_us() - t_setup) / 1000;
            match result {
                Ok(()) => {
                    shown += 1;
                    let bytes = (w * h * cfg.controller.bytes_per_pixel()) as u64;
                    kprintln!(
                        "lcd: probe {n}/{count} {}: done in {took} ms (text card {bytes} bytes in {text_ms} ms = {} kbit/s measured, {} kbit/s from the divider)",
                        desc.as_str(),
                        bytes * 8 / text_ms.max(1),
                        core_hz / cdiv / 1000
                    );
                }
                Err(at) => {
                    skipped += 1;
                    kprintln!(
                        "lcd: probe {n}/{count} {}: skipped after {took} ms, the SPI transfers failed at the {}",
                        desc.as_str(),
                        at.name()
                    );
                }
            }
        }
    }
    kprintln!(
        "lcd: probe finished in {} ms: {shown} set-up(s) shown, {skipped} skipped; no display driver is running; put the options of the set-up you could see into cmdline.txt in place of milk.lcd=probe",
        (now_us() - t_all) / 1000
    );
}

/// The panel was brought up and is in use.
pub fn ready() -> bool {
    STATE.load(Ordering::Acquire) == READY
}

/// The panel is the screen even if the firmware has a framebuffer (`milk.hdmi=off`).
pub fn primary_wanted() -> bool {
    settings().is_some_and(|s| s.hdmi_off)
}

/// Interrupts are on from here (see [`IRQS_LIVE`]).
pub fn irqs_live() {
    IRQS_LIVE.store(true, Ordering::Release);
}

// ---- sending ------------------------------------------------------------------------------------

fn full_rect(s: &Settings) -> Rect {
    Rect::new(0, 0, s.cfg.width(), s.cfg.height())
}

/// Note that the panel's rectangle `r` must be sent.
fn mark(r: Rect) {
    if let Some(i) = INNER.lock().as_mut()
        && let Some(s) = settings()
    {
        i.dirty.add(r.clip(s.cfg.width(), s.cfg.height()));
    }
}

/// The console changed pixel rows `[lo, hi)` of the panel picture.
pub fn mark_rows(lo: usize, hi: usize) {
    if let Some(s) = settings() {
        mark(Rect::new(0, lo, s.cfg.width(), hi.saturating_sub(lo)));
    }
}

fn mark_all() {
    if let Some(s) = settings() {
        mark(full_rect(&s));
    }
}

/// Rectangles of the desktop's framebuffer that changed (the compositor's `present`): while the
/// desktop is on the panel, the panel rectangles that show them are marked.
pub fn note_source(rects: &[crate::gfx::Rect]) {
    if MODE.load(Ordering::Relaxed) != DESKTOP {
        return;
    }
    let Some(s) = settings() else { return };
    let mut g = INNER.lock();
    let Some(i) = g.as_mut() else { return };
    for r in rects {
        if r.w > 0 && r.h > 0 && r.x >= 0 && r.y >= 0 {
            let m = i.view.map_rect(Rect::new(
                r.x as usize,
                r.y as usize,
                r.w as usize,
                r.h as usize,
            ));
            i.dirty.add(m.clip(s.cfg.width(), s.cfg.height()));
        }
    }
}

/// The whole desktop framebuffer may have changed.
pub fn note_source_all() {
    if MODE.load(Ordering::Relaxed) == DESKTOP {
        mark_all();
    }
}

/// Rows of the panel per SPI window: a band is a few milliseconds on the wire, and the bus is
/// released between bands, so the touch chip is never kept waiting long.
const BAND_ROWS: usize = 16;

/// Send what is dirty, in bands, for at most `budget_us` (what is left stays dirty). `pump`: the
/// print path, which may only use the bus if it is free. Returns whether anything was sent.
fn flush(pump: bool, budget_us: u64) -> bool {
    let Some(s) = settings() else { return false };
    if CRASHED.load(Ordering::Acquire) {
        return false;
    }
    let mut rects = [Rect::default(); 4];
    let mut n = 0;
    let view;
    {
        let mut g = INNER.lock();
        let Some(i) = g.as_mut() else { return false };
        if i.dirty.is_empty() {
            return false;
        }
        while let Some(r) = i.dirty.pop() {
            rects[n] = r;
            n += 1;
        }
        view = i.view;
    }
    let t0 = now_us();
    let (mut bands, mut ok) = (0usize, true);
    'rects: for ri in 0..n {
        let r = rects[ri];
        let mut y = r.y;
        while y < r.y1() {
            // Out of time, or the bus is taken: what is left goes back on the list.
            let bus = if now_us().saturating_sub(t0) >= budget_us {
                None
            } else {
                lock_bus(if pump { 0 } else { 30 })
            };
            let Some(bus) = bus else {
                mark(Rect::new(r.x, y, r.w, r.y1() - y));
                for rest in &rects[ri + 1..n] {
                    mark(*rest);
                }
                break 'rects;
            };
            let band = Rect::new(r.x, y, r.w, BAND_ROWS.min(r.y1() - y));
            ok = send(&s, &[band], view);
            drop(bus);
            if !ok {
                break 'rects;
            }
            bands += 1;
            y += band.h;
        }
    }
    if bands == 0 && ok {
        return false;
    }
    let t1 = now_us();
    if ok {
        FAILS.store(0, Ordering::Relaxed);
        if MODE.load(Ordering::Relaxed) == DESKTOP && !FIRST_FRAME.swap(true, Ordering::Relaxed) {
            kprintln!(
                "lcd: first desktop update on the panel: {n} rectangle(s), {bands} band(s), {} ms",
                (t1 - t0) / 1000
            );
        }
    } else if FAILS.fetch_add(1, Ordering::Relaxed) >= 2 {
        STATE.store(FAILED, Ordering::Release);
        kprintln!("lcd: three SPI transfers in a row timed out, the panel is off");
    }
    true
}

/// Is there something left to send?
fn dirty_left() -> bool {
    INNER.lock().as_ref().is_some_and(|i| !i.dirty.is_empty())
}

/// Draw `rects` of the panel from what the mode says: the shadow (text) or the desktop through
/// the view. No allocation.
fn send(s: &Settings, rects: &[Rect], view: View) -> bool {
    let mut bus = HwBus {
        dc: s.dc,
        rst: s.rst,
        cdiv: s.cdiv,
        mode: s.mode,
    };
    let (w, h) = (s.cfg.width(), s.cfg.height());
    if MODE.load(Ordering::Relaxed) == TEXT {
        // SAFETY: the static shadow, w * h words; the console may be drawing on it, which at worst
        // shows a half-drawn line for one frame.
        let px = unsafe { core::slice::from_raw_parts(SHADOW.0.get() as *const u32, w * h) };
        rects
            .iter()
            .all(|r| pk::draw_rect(&s.cfg, &mut bus, *r, &mut Pixels { px, width: w }))
    } else {
        let Some(fb) = display::framebuffer() else {
            return true;
        };
        let (sw, sh) = display::size();
        if view.source() != (sw, sh) {
            return true; // the mode changed under us: the thread rebuilds the view and marks all
        }
        // SAFETY: the display's framebuffer, stride = width, `sw * sh` words, mapped for good.
        let src = unsafe { core::slice::from_raw_parts(fb as *const u32, sw * sh) };
        rects.iter().all(|r| {
            pk::draw_rect(
                &s.cfg,
                &mut bus,
                *r,
                &mut ViewSource {
                    view,
                    src,
                    stride: sw,
                },
            )
        })
    }
}

/// The most one `kprintln!` spends on the panel.
const PUMP_BUDGET_US: u64 = 25_000;

/// From `kprintln!`, outside the print lock: send what the log console changed, where that is
/// safe and cheap enough.
pub fn pump() {
    if STATE.load(Ordering::Relaxed) != READY
        || MODE.load(Ordering::Relaxed) != TEXT
        || CRASHED.load(Ordering::Relaxed)
    {
        return;
    }
    if IRQS_LIVE.load(Ordering::Relaxed) && !crate::sync::irqs_enabled() {
        return;
    }
    flush(true, PUMP_BUDGET_US);
}

// ---- the boot console and the crash screen -------------------------------------------------------

/// The kernel log on the panel, as early as possible: replay what was logged so far on a text
/// console of the panel's own size and keep `kprintln!` going there until the desktop starts.
pub fn boot_console() {
    let Some(s) = settings() else { return };
    let mut console = Console::with_scale(shadow_surface(&s.cfg), 1);
    console.set_wrap(true);
    crate::klog::read_since(0, |bytes| {
        for chunk in bytes.utf8_chunks() {
            console.write_str(chunk.valid());
        }
    });
    let _ = console.take_dirty();
    sink::install_lcd_console(console);
    mark_all();
    let t = now_us();
    flush(false, u64::MAX);
    kprintln!(
        "lcd: boot log on the panel (first full picture {} ms)",
        (now_us() - t) / 1000
    );
}

/// Draw the crash screen on the panel: the end of the log, through `f`, on a plain scrolling
/// console, and send all of it now. Works with interrupts off, locks possibly held and other CPUs
/// running; allocates nothing. False if there is no panel.
pub fn crash_paint(f: impl FnOnce(&mut Console)) -> bool {
    let Some(s) = settings() else { return false };
    CRASHED.store(true, Ordering::SeqCst);
    MODE.store(TEXT, Ordering::SeqCst);
    // Someone may be in the middle of a transfer (another CPU): give it a moment, then take the
    // bus anyway and clear the controller.
    let _guard = lock_bus(30);
    BUS.store(true, Ordering::Release);
    spi::reset();
    let mut console = Console::with_scale(shadow_surface(&s.cfg), 1);
    f(&mut console);
    let _ = console.take_dirty();
    let mut bus = HwBus {
        dc: s.dc,
        rst: s.rst,
        cdiv: s.cdiv,
        mode: s.mode,
    };
    let (w, h) = (s.cfg.width(), s.cfg.height());
    // SAFETY: the static shadow just drawn on.
    let px = unsafe { core::slice::from_raw_parts(SHADOW.0.get() as *const u32, w * h) };
    pk::draw_rect(
        &s.cfg,
        &mut bus,
        full_rect(&s),
        &mut Pixels { px, width: w },
    );
    BUS.store(false, Ordering::Release);
    true
}

// ---- the desktop on the panel --------------------------------------------------------------------

fn make_view(s: &Settings, sw: usize, sh: usize) -> View {
    let (w, h) = (s.cfg.width(), s.cfg.height());
    if s.pan {
        View::cropped(sw, sh, w, h)
    } else {
        View::scaled(sw, sh, w, h)
    }
}

/// The desktop has the framebuffer from now on: the panel shows it (scaled or cropped) and stops
/// being the boot log. Waits (bounded) for the calibration helper first.
pub fn desktop_started() {
    let Some(s) = settings() else { return };
    let t = Timeout::ms(120_000);
    while CAL_BUSY.load(Ordering::Acquire) && !t.expired() {
        crate::sched::sleep_ms(50);
    }
    sink::remove_lcd_console();
    let (sw, sh) = display::size();
    if sw == 0 || sh == 0 {
        return;
    }
    if let Some(i) = INNER.lock().as_mut() {
        i.view = make_view(&s, sw, sh);
        i.dirty.clear();
    }
    MODE.store(DESKTOP, Ordering::SeqCst);
    mark_all();
    let (w, h) = (s.cfg.width(), s.cfg.height());
    kprintln!(
        "lcd: the desktop ({sw}x{sh}) is on the panel {}x{}, {}",
        w,
        h,
        if s.pan {
            "1:1, touch the edge to move the view"
        } else {
            "scaled down"
        }
    );
}

/// The desktop framebuffer changed size (or was not looked at yet): lay the view out again.
fn ensure_view(s: &Settings) {
    let (sw, sh) = display::size();
    if sw == 0 || sh == 0 {
        return;
    }
    let mut changed = false;
    if let Some(i) = INNER.lock().as_mut()
        && i.view.source() != (sw, sh)
    {
        i.view = make_view(s, sw, sh);
        changed = true;
    }
    if changed {
        mark_all();
    }
}

// ---- the panel as the screen (no HDMI) -----------------------------------------------------------

/// Pixels of the RAM framebuffer the desktop draws on when the panel is the screen: room for the
/// biggest display mode (1440x900).
const PRIMARY_MAX_PIXELS: usize = 1440 * 900;
static PRIMARY_PA: AtomicUsize = AtomicUsize::new(0);
static PRIMARY_W: AtomicUsize = AtomicUsize::new(0);
static PRIMARY_H: AtomicUsize = AtomicUsize::new(0);

fn primary_surface_of(w: usize, h: usize) -> Option<Surface<'static>> {
    if w * h > PRIMARY_MAX_PIXELS || w == 0 || h == 0 {
        return None;
    }
    if PRIMARY_PA.load(Ordering::Acquire) == 0 {
        let pages = (PRIMARY_MAX_PIXELS * 4).div_ceil(crate::mmu::PAGE_SIZE);
        let pa = crate::pmm::alloc_contiguous_zeroed_as(crate::pmm::Use::Display, pages, 1)?;
        PRIMARY_PA.store(pa, Ordering::Release);
    }
    PRIMARY_W.store(w, Ordering::Release);
    PRIMARY_H.store(h, Ordering::Release);
    let va = crate::mmu::phys_to_virt(PRIMARY_PA.load(Ordering::Acquire));
    // SAFETY: frames we own for good, mapped in the linear map, `w * h <= PRIMARY_MAX_PIXELS` words.
    Some(unsafe { Surface::from_raw(va as *mut u32, w, h, w) })
}

/// The framebuffer of a panel that is the screen: `w` x `h` XRGB in RAM.
pub fn primary_surface(w: usize, h: usize) -> Option<Surface<'static>> {
    primary_surface_of(w, h)
}

pub fn primary_set_mode(w: usize, h: usize) -> Option<Surface<'static>> {
    primary_surface_of(w, h)
}

pub fn primary_size() -> (usize, usize) {
    (
        PRIMARY_W.load(Ordering::Acquire),
        PRIMARY_H.load(Ordering::Acquire),
    )
}

pub fn primary_framebuffer() -> Option<*mut u32> {
    let pa = PRIMARY_PA.load(Ordering::Acquire);
    (pa != 0).then(|| crate::mmu::phys_to_virt(pa) as *mut u32)
}

// ---- the thread: touch, and the desktop's rectangles ----------------------------------------------

/// Start the panel's thread (touch polling, sending the desktop). After the scheduler is up.
pub fn start() {
    if ready() {
        CAL_BUSY.store(
            settings().is_some_and(|s| s.touch && s.cal_ask),
            Ordering::Release,
        );
        crate::sched::spawn("lcd", thread);
    }
}

fn thread() {
    let Some(s) = settings() else { return };
    let mut cal = s.cal;
    if s.touch && s.cal_ask {
        if let Some(c) = calibrate(&s) {
            cal = c;
        }
        CAL_BUSY.store(false, Ordering::Release);
    }
    let mut tracker = Tracker::new();
    let mut tick = 0u32;
    loop {
        // Touch every 10 ms; while there is something to send, the panel gets the time in between.
        crate::sched::sleep_ms(if dirty_left() { 1 } else { 10 });
        tick = tick.wrapping_add(1);
        if !ready() || CRASHED.load(Ordering::Acquire) {
            continue;
        }
        if s.touch {
            poll_touch(&s, &cal, &mut tracker, tick);
        }
        if MODE.load(Ordering::Relaxed) == DESKTOP {
            ensure_view(&s);
        }
        // The desktop is sent in the thread's time, a few milliseconds at a time.
        flush(false, 8_000);
    }
}

/// One reading of the touch chip, in panel pixels (None: no touch, or the bus is busy and this
/// tick does not count).
fn read_touch(s: &Settings, cal: &Cal) -> Option<Option<(i32, i32)>> {
    let _bus = lock_bus(5)?;
    let t = touch::read(&mut TouchHw { cdiv: s.touch_cdiv });
    Some(t.map(|t| {
        let (x, y) = cal.apply(t.x as i32, t.y as i32);
        (
            x.clamp(0, s.cfg.width() as i32 - 1),
            y.clamp(0, s.cfg.height() as i32 - 1),
        )
    }))
}

fn poll_touch(s: &Settings, cal: &Cal, tr: &mut Tracker, tick: u32) {
    // The pen-down line is low while something presses; if it is not wired or stuck, the chip is
    // still asked every tenth tick, so touches are found (slowly) either way.
    let due = match s.irq.map(|p| !spi::read(p)) {
        Some(true) | None => true,
        Some(false) => tr.is_down() || tick % 10 == 0,
    };
    let sample = if due {
        match read_touch(s, cal) {
            Some(v) => v,
            None => return,
        }
    } else {
        None
    };
    let ev = tr.update(sample);
    if MODE.load(Ordering::Relaxed) != DESKTOP {
        return;
    }
    let to_pointer = |lx: i32, ly: i32| {
        let src = INNER.lock().as_ref().map(|i| i.view.to_source(lx, ly));
        if let Some((x, y)) = src {
            input::report_pointer(x, y);
        }
    };
    match ev {
        Some(Event::Down(x, y)) => {
            to_pointer(x, y);
            input::report_button(MouseButton::Left, true);
        }
        Some(Event::Move(x, y)) => to_pointer(x, y),
        Some(Event::Up) => input::report_button(MouseButton::Left, false),
        None => {}
    }
    // A cropped view follows a finger held near the edge of the panel.
    if s.pan && tr.is_down() {
        let (x, y) = tr.position();
        let moved = INNER
            .lock()
            .as_mut()
            .is_some_and(|i| i.view.pan_towards(x, y, 24, 12));
        if moved {
            mark_all();
            to_pointer(x, y);
        }
    }
}

// ---- the calibration helper (milk.touch_cal=ask) --------------------------------------------------

/// Four crosses, one after the other; the touches are fitted to an affine map, shown on the panel
/// and logged as the option to put in cmdline.txt. Runs before the desktop starts; gives up
/// (keeping the default) if nobody touches for a minute.
fn calibrate(s: &Settings) -> Option<Cal> {
    use crate::font;
    use crate::gfx::{DrawTarget, rgb};
    let (w, h) = (s.cfg.width() as i32, s.cfg.height() as i32);
    let targets = touch::cal_targets(w, h);
    sink::remove_lcd_console();
    let bg = rgb(0x16, 0x15, 0x13);
    let fg = rgb(0xec, 0xe6, 0xd8);
    let hot = rgb(0xf2, 0xd2, 0x7e);
    let mut raws = [(0i32, 0i32); 4];
    let show = |lines: &[&str], cross: Option<(i32, i32)>| {
        let mut surf = shadow_surface(&s.cfg);
        surf.clear(bg);
        for (i, l) in lines.iter().enumerate() {
            font::draw_str_alpha(&mut surf, 24, 100 + 20 * i as i32, l, fg, 255, 1);
        }
        if let Some((x, y)) = cross {
            surf.fill_rect(x - 14, y - 1, 29, 3, hot);
            surf.fill_rect(x - 1, y - 14, 3, 29, hot);
        }
        mark_all();
        flush(false, u64::MAX);
    };
    kprintln!("touch: calibration, touch the four crosses (milk.touch_cal=ask)");
    for (i, t) in targets.iter().enumerate() {
        let title = [
            "Touch the cross: 1 of 4",
            "Touch the cross: 2 of 4",
            "Touch the cross: 3 of 4",
            "Touch the cross: 4 of 4",
        ][i];
        show(
            &[title, "(press firmly with a stylus or a fingernail)"],
            Some(*t),
        );
        let mut agree = 0;
        let mut last = (0i32, 0i32);
        let mut sum = (0i32, 0i32);
        let give_up = Timeout::ms(60_000);
        loop {
            if give_up.expired() {
                kprintln!("touch: nobody touched, keeping the default calibration");
                return None;
            }
            crate::sched::sleep_ms(20);
            let Some(_bus) = lock_bus(5) else { continue };
            match touch::read(&mut TouchHw { cdiv: s.touch_cdiv }) {
                Some(t) => {
                    let p = (t.x as i32, t.y as i32);
                    if agree > 0 && ((p.0 - last.0).abs() > 120 || (p.1 - last.1).abs() > 120) {
                        agree = 0;
                        sum = (0, 0);
                    }
                    agree += 1;
                    last = p;
                    sum = (sum.0 + p.0, sum.1 + p.1);
                    if agree >= 8 {
                        raws[i] = (sum.0 / agree, sum.1 / agree);
                        break;
                    }
                }
                None => {
                    agree = 0;
                    sum = (0, 0);
                }
            }
        }
        kprintln!("touch: cross {} raw {},{}", i + 1, raws[i].0, raws[i].1);
        show(&["Thank you, lift your finger"], None);
        // Wait for the lift.
        let lift = Timeout::ms(5_000);
        while !lift.expired() {
            crate::sched::sleep_ms(30);
            if let Some(_bus) = lock_bus(5)
                && touch::read(&mut TouchHw { cdiv: s.touch_cdiv }).is_none()
            {
                break;
            }
        }
    }
    let cal = touch::fit(&raws, &targets);
    let mut text = [0u8; 96];
    match cal {
        Some(c) => {
            let n = c.write(&mut text);
            let t = core::str::from_utf8(&text[..n]).unwrap_or("?");
            kprintln!("touch: add this to cmdline.txt: milk.touch_cal={t}");
            let line = alloc::format!("milk.touch_cal={t}");
            let mut surf = shadow_surface(&s.cfg);
            surf.clear(bg);
            font::draw_str_alpha(
                &mut surf,
                24,
                80,
                "Calibrated. Put this in cmdline.txt:",
                fg,
                255,
                1,
            );
            font::draw_str_alpha(&mut surf, 24, 110, &line, hot, 255, 1);
            font::draw_str_alpha(
                &mut surf,
                24,
                150,
                "(replacing milk.touch_cal=ask)",
                fg,
                255,
                1,
            );
            mark_all();
            flush(false, u64::MAX);
            // Long enough to copy it down.
            crate::sched::sleep_ms(12_000);
        }
        None => kprintln!("touch: the four touches do not give a calibration, keeping the default"),
    }
    // The boot log comes back (the klog has everything, including the lines above).
    boot_console();
    cal
}
