//! The touch chip of an HDMI display (docs/raspberry-pi.md, "HDMI displays"): an XPT2046 on SPI0, with
//! no SPI panel in the way. The 4-inch 800x480 HDMI kits show their picture over HDMI and use the GPIO
//! header only for 5 V and for this chip (SPI0, chip select 1, pen-down line on GPIO 17).
//!
//! Off unless the command line says `milk.touch=xpt2046` and names no SPI panel (`milk.lcd=`: that
//! driver, rpi/lcd.rs, reads the touch chip itself when it is there). Options:
//!
//! * `milk.touch_cs=0|1` (default 1: the chip select the chip is wired to), `milk.touch_irq=` (17, `-1`
//!   for polling only), `milk.touch_cal=a,b,c,d,e,f` or `ask` (the four-cross helper), `milk.touch_rot=0|90|180|270`
//!   (how the glass is fitted when there is no calibration: default 0), `milk.touch_hz=` (1000000, at most 2000000).
//!
//! Only the pins the chip needs are touched: MISO, MOSI, SCLK, the chosen chip select and the pen line;
//! never the display's pins (the other chip select, D/C, reset). The thread starts after the boot is
//! over and the boot log is on the card (it can never delay the boot), except with `milk.touch_cal=ask`,
//! where the helper has the screen for the four touches before the desktop starts (it gives up after a
//! minute without one). Positions go to the input layer in the pixels of the framebuffer.
//! Nothing here may hang or panic when the chip is absent: SPI0 is probed first, every transfer is
//! bounded, and a chip that answers with nothing is simply "no touch".

use core::sync::atomic::{AtomicBool, Ordering};

use rpikit::cmdline;
use rpikit::lcd::Rotation;
use rpikit::lcdbudget::ProbeGate;
use rpikit::touch::{self as pk, Cal, Event, Tracker};

use super::{Timeout, lcd, spi};
use crate::input::{self, MouseButton};
use crate::{display, kprintln};

/// The helper is running: the desktop waits for it.
static CAL_BUSY: AtomicBool = AtomicBool::new(false);

/// With no log file on the card to wait for, the thread starts this long after the boot is over.
const GRACE_MS: u64 = 8_000;

struct Settings {
    cs: u8,
    irq: Option<u8>,
    hz: u32,
    cal: Option<Cal>,
    cal_ask: bool,
    turn: Rotation,
}

fn option(name: &str) -> Option<&'static [u8]> {
    crate::fwcfg::option(name)
}

/// Is the standalone touch driver asked for (and not made redundant by an SPI panel)? Says why not.
fn wanted() -> bool {
    let Some(v) = option("touch") else {
        return false;
    };
    if !v.eq_ignore_ascii_case(b"xpt2046") {
        // 0 / 1 / on / off are the SPI panel driver's switch for its own touch chip.
        return false;
    }
    if lcd::configured() {
        kprintln!(
            "touch: milk.touch=xpt2046 with milk.lcd set: the SPI panel driver reads the touch chip, nothing extra started"
        );
        return false;
    }
    true
}

fn pin(name: &str, default: u8) -> u8 {
    match option(name).and_then(cmdline::parse_num) {
        None => default,
        Some(n) => match spi::header_pin(n) {
            Some(p) => p,
            None => {
                kprintln!("touch: milk.{name}={n} is not a header GPIO (2..27), using {default}");
                default
            }
        },
    }
}

fn parse_settings() -> Settings {
    let cs = match option("touch_cs") {
        None => 1,
        Some(b"0") => 0,
        Some(b"1") => 1,
        Some(_) => {
            kprintln!("touch: milk.touch_cs must be 0 or 1, using 1");
            1
        }
    };
    let irq = match option("touch_irq") {
        Some(b"-1" | b"none" | b"off") => None,
        _ => Some(pin("touch_irq", 17)),
    };
    let hz = option("touch_hz")
        .and_then(cmdline::parse_num)
        .filter(|&v| v > 0)
        .map_or(1_000_000, |v| (v as u32).min(pk::MAX_HZ));
    let cal_opt = option("touch_cal");
    let cal_ask = cal_opt == Some(b"ask");
    let cal = cal_opt.filter(|_| !cal_ask).and_then(|v| {
        let c = Cal::parse(v);
        if c.is_none() {
            kprintln!("touch: milk.touch_cal needs six numbers a,b,c,d,e,f (or ask), using the default");
        }
        c
    });
    let turn = match option("touch_rot") {
        None => Rotation::Deg0,
        Some(v) => Rotation::parse(v).unwrap_or_else(|| {
            kprintln!("touch: milk.touch_rot must be 0, 90, 180 or 270, using 0");
            Rotation::Deg0
        }),
    };
    Settings {
        cs,
        irq,
        hz,
        cal,
        cal_ask,
        turn,
    }
}

/// The chip on the chosen chip select.
struct Chip {
    cs: u8,
    cdiv: u32,
}

impl pk::TouchBus for Chip {
    fn transfer(&mut self, tx: &[u8], rx: &mut [u8]) -> bool {
        spi::transfer(self.cs, self.cdiv, tx, Some(rx))
    }
}

/// Start the touch thread if the command line asks for it. After the scheduler is up.
pub fn start() {
    if !wanted() {
        return;
    }
    let s = parse_settings();
    CAL_BUSY.store(s.cal_ask, Ordering::Release);
    crate::sched::spawn_prio("touch", crate::sched::Priority::Low, move || thread(s));
}

/// The desktop is about to take the screen: wait (bounded) for the calibration helper.
pub fn desktop_starting() {
    let t = Timeout::ms(120_000);
    while CAL_BUSY.load(Ordering::Acquire) && !t.expired() {
        crate::sched::sleep_ms(50);
    }
}

fn thread(s: Settings) {
    if !s.cal_ask {
        // After the boot is over and the log is saved: never in the way of either.
        let mut gate = ProbeGate::new(GRACE_MS);
        let t0 = timer_ms();
        loop {
            if gate.ready(
                timer_ms() - t0,
                super::led::finished(),
                super::logfile::saved_quiet(),
            ) {
                break;
            }
            crate::sched::sleep_ms(250);
        }
    }
    let (core_hz, core_src) = lcd::core_clock();
    spi::set_core_hz(core_hz);
    let cdiv = spi::divider_for(core_hz, s.hz);
    let (nw, nh) = display::size();
    let (nw, nh) = (nw.max(1) as i32, nh.max(1) as i32);
    let ce_pin = if s.cs == 0 { 8 } else { 7 };
    kprintln!(
        "touch: XPT2046 on SPI0 CE{} (GPIO {ce_pin}), MISO GPIO 9, MOSI GPIO 10, SCLK GPIO 11, {} kHz (core {} MHz {core_src}), pen IRQ {}; the display's pins (the other chip select, D/C, reset) are not touched",
        s.cs,
        core_hz / cdiv / 1000,
        core_hz / 1_000_000,
        match s.irq {
            Some(p) => alloc::format!("GPIO {p}"),
            None => alloc::string::String::from("none (polled)"),
        }
    );
    spi::connect_pins_for_touch(s.cs);
    if let Some(p) = s.irq {
        spi::set_function(p, spi::Func::Input);
        spi::set_pull(p, spi::Pull::Up);
    }
    if !spi::present() {
        let (cs, clk) = spi::registers();
        kprintln!("touch: SPI0 does not answer (no such controller here?): no touch (CS {cs:#x} CLK {clk:#x})");
        CAL_BUSY.store(false, Ordering::Release);
        return;
    }
    spi::reset();
    let mut chip = Chip { cs: s.cs, cdiv };
    let mut cal = s.cal.unwrap_or_else(|| Cal::default_for_screen(nw, nh, s.turn));
    if s.cal_ask {
        if let Some(c) = calibrate(&mut chip, nw, nh) {
            cal = c;
        }
        CAL_BUSY.store(false, Ordering::Release);
    }
    let mut text = [0u8; 96];
    let n = cal.write(&mut text);
    kprintln!(
        "touch: running, map raw -> {nw}x{nh} pixels {}: milk.touch_cal={} (rot {})",
        if s.cal.is_some() { "(from the command line)" } else { "(default)" },
        core::str::from_utf8(&text[..n]).unwrap_or("?"),
        s.turn.degrees()
    );
    let mut tr = Tracker::new();
    let mut tick = 0u32;
    let mut first = 0u32;
    loop {
        crate::sched::sleep_ms(10);
        tick = tick.wrapping_add(1);
        // The pen-down line is low while something presses; if it is not wired or stuck, the chip is
        // still asked every tenth tick, so touches are found (slowly) either way.
        let due = match s.irq.map(|p| !spi::read(p)) {
            Some(true) | None => true,
            Some(false) => tr.is_down() || tick % 10 == 0,
        };
        let sample = if due {
            pk::read(&mut chip).map(|t| {
                let (x, y) = cal.apply(t.x as i32, t.y as i32);
                let p = (x.clamp(0, nw - 1), y.clamp(0, nh - 1));
                if first < 3 && !tr.is_down() {
                    first += 1;
                    kprintln!(
                        "touch: touch {first}: raw {},{} pressure {} -> pixel {},{}",
                        t.x,
                        t.y,
                        t.pressure,
                        p.0,
                        p.1
                    );
                }
                p
            })
        } else {
            None
        };
        // The desktop's pointer space follows the display mode: scale from the framebuffer's pixels.
        let (sw, sh) = input::screen_size();
        let to = |x: i32, y: i32| (x * sw / nw, y * sh / nh);
        match tr.update(sample) {
            Some(Event::Down(x, y)) => {
                let (x, y) = to(x, y);
                input::report_pointer(x, y);
                input::report_button(MouseButton::Left, true);
            }
            Some(Event::Move(x, y)) => {
                let (x, y) = to(x, y);
                input::report_pointer(x, y);
            }
            Some(Event::Up) => input::report_button(MouseButton::Left, false),
            None => {}
        }
    }
}

fn timer_ms() -> u64 {
    crate::timer::now_ns() / 1_000_000
}

/// Four crosses on the framebuffer, one after the other; the touches are fitted to an affine map
/// and logged as the option to put in cmdline.txt. The boot console steps aside for it (the desktop
/// takes the screen when it is done). Gives up (keeping the default) after a minute without a touch.
fn calibrate(chip: &mut Chip, w: i32, h: i32) -> Option<Cal> {
    use crate::font;
    use crate::gfx::{DrawTarget, Surface, rgb};
    let fbp = display::framebuffer()?;
    if crate::sink::screen_installed() {
        crate::sink::remove_screen();
    }
    // SAFETY: the display's own framebuffer (w * h words), which nothing else draws on while the
    // desktop waits for this helper.
    let mut surf = unsafe { Surface::from_raw(fbp, w as usize, h as usize, w as usize) };
    let targets = pk::cal_targets(w, h);
    let bg = rgb(0x16, 0x15, 0x13);
    let fg = rgb(0xec, 0xe6, 0xd8);
    let hot = rgb(0xf2, 0xd2, 0x7e);
    let mut raws = [(0i32, 0i32); 4];
    kprintln!("touch: calibration, touch the four crosses (milk.touch_cal=ask)");
    let mut show = |lines: &[&str], cross: Option<(i32, i32)>| {
        surf.clear(bg);
        for (i, l) in lines.iter().enumerate() {
            font::draw_str_alpha(&mut surf, 40, h / 2 - 40 + 24 * i as i32, l, fg, 255, 2);
        }
        if let Some((x, y)) = cross {
            surf.fill_rect(x - 20, y - 1, 41, 3, hot);
            surf.fill_rect(x - 1, y - 20, 3, 41, hot);
        }
    };
    for (i, t) in targets.iter().enumerate() {
        let title = [
            "Touch the cross: 1 of 4",
            "Touch the cross: 2 of 4",
            "Touch the cross: 3 of 4",
            "Touch the cross: 4 of 4",
        ][i];
        show(&[title, "(press firmly with a stylus or a fingernail)"], Some(*t));
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
            match pk::read(chip) {
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
        let lift = Timeout::ms(5_000);
        while !lift.expired() {
            crate::sched::sleep_ms(30);
            if pk::read(chip).is_none() {
                break;
            }
        }
    }
    let cal = pk::fit(&raws, &targets);
    let mut text = [0u8; 96];
    match cal {
        Some(c) => {
            let n = c.write(&mut text);
            let t = core::str::from_utf8(&text[..n]).unwrap_or("?");
            kprintln!("touch: add this to cmdline.txt: milk.touch_cal={t}");
            let line = alloc::format!("milk.touch_cal={t}");
            surf.clear(bg);
            font::draw_str_alpha(&mut surf, 40, h / 2 - 40, "Calibrated. Put this in cmdline.txt:", fg, 255, 2);
            font::draw_str_alpha(&mut surf, 40, h / 2, &line, hot, 255, 1);
            font::draw_str_alpha(&mut surf, 40, h / 2 + 40, "(replacing milk.touch_cal=ask)", fg, 255, 2);
            // Long enough to copy it down.
            crate::sched::sleep_ms(12_000);
        }
        None => kprintln!("touch: the four touches do not give a calibration, keeping the default"),
    }
    cal
}
