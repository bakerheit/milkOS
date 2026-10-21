//! Shapes the GUI library does not have: thick lines, rings and arcs (integer maths, soft edges),
//! and the sin/cos the clock hands need.

use gui::{Color, Window, blend};

pub fn isqrt(n: u64) -> u64 {
    if n < 2 {
        return n;
    }
    let mut x = n;
    let mut y = x.div_ceil(2);
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}

const PI: f64 = core::f64::consts::PI;

/// sin(x), x in radians (odd Taylor series after folding; error below 1e-7).
pub fn sin(x: f64) -> f64 {
    let two_pi = 2.0 * PI;
    let mut x = x - two_pi * ((x / two_pi) as i64 as f64);
    if x > PI {
        x -= two_pi;
    } else if x < -PI {
        x += two_pi;
    }
    if x > PI / 2.0 {
        x = PI - x;
    } else if x < -PI / 2.0 {
        x = -PI - x;
    }
    let x2 = x * x;
    let mut term = x;
    let mut sum = x;
    for k in 1..8 {
        term *= -x2 / ((2 * k) as f64 * (2 * k + 1) as f64);
        sum += term;
    }
    sum
}

pub fn cos(x: f64) -> f64 {
    sin(x + PI / 2.0)
}

/// Where the point at `turns` (0..1, clockwise from 12 o'clock) and distance `r` from (cx, cy) is.
pub fn on_circle(cx: i32, cy: i32, r: f64, turns: f64) -> (i32, i32) {
    let a = turns * 2.0 * PI;
    (
        cx + (sin(a) * r + if sin(a) >= 0.0 { 0.5 } else { -0.5 }) as i32,
        cy - (cos(a) * r + if cos(a) >= 0.0 { 0.5 } else { -0.5 }) as i32,
    )
}

/// The angle of the vector (dx, dy) in screen coordinates (y down), clockwise from straight up,
/// as 0..65536 for a full turn. Accurate to about 0.2 degrees.
pub fn clock_angle(dx: i32, dy: i32) -> u32 {
    let (ux, uy) = (dx as i64, -(dy as i64));
    if ux == 0 && uy == 0 {
        return 0;
    }
    let (ax, ay) = (ux.abs(), uy.abs());
    // atan(min/max) in 1/65536 turns, from atan(r) ~ r (pi/4 + 0.273 (1 - r)).
    let atan = |num: i64, den: i64| -> i64 {
        let r = num * 32768 / den; // Q15
        let term = 25736 + 8946 * (32768 - r) / 32768;
        (r * term / 32768) * 10430 / 32768
    };
    let phi = if ax <= ay {
        atan(ax, ay)
    } else {
        16384 - atan(ay, ax)
    };
    let a = match (ux >= 0, uy >= 0) {
        (true, true) => phi,
        (true, false) => 32768 - phi,
        (false, false) => 32768 + phi,
        (false, true) => 65536 - phi,
    };
    (a.rem_euclid(65536)) as u32
}

/// Coverage 0..=255 of a pixel at distance `d_q4` (sixteenths of a pixel from the centre of the
/// pixel) for a disc of radius `r_q4`, with a one pixel soft edge.
fn edge(r_q4: i64, d_q4: i64) -> i64 {
    ((r_q4 - d_q4 + 8).clamp(0, 16)) * 255 / 16
}

/// A ring between radii `r_in` and `r_out` centred on (cx, cy). If `sweep` is `Some(turns)` only
/// the part from 12 o'clock clockwise for that fraction of a turn is drawn.
pub fn ring(
    win: &mut Window,
    cx: i32,
    cy: i32,
    r_in: i32,
    r_out: i32,
    sweep: Option<f64>,
    c: Color,
) {
    let end = sweep.map(|t| (t.clamp(0.0, 1.0) * 65536.0) as u32);
    if end == Some(0) {
        return;
    }
    let (w, h) = (win.w, win.h);
    let stride = w as usize;
    let (ri, ro) = (r_in as i64 * 16, r_out as i64 * 16);
    for y in (cy - r_out - 1).max(0)..(cy + r_out + 2).min(h) {
        for x in (cx - r_out - 1).max(0)..(cx + r_out + 2).min(w) {
            let (dx, dy) = ((x - cx) as i64, (y - cy) as i64);
            let d_q4 = isqrt((dx * dx + dy * dy) as u64 * 256) as i64;
            let a = edge(ro, d_q4).min(255 - edge(ri, d_q4)).min(255);
            if a <= 0 {
                continue;
            }
            if let Some(e) = end
                && (x != cx || y != cy)
                && clock_angle(dx as i32, dy as i32) > e
            {
                continue;
            }
            let p = &mut win.pixels()[y as usize * stride + x as usize];
            *p = blend(*p, c, a as u8);
        }
    }
}

/// A line of thickness `t` pixels from (x0, y0) to (x1, y1) with square-ish soft ends.
pub fn line(win: &mut Window, x0: i32, y0: i32, x1: i32, y1: i32, t: i32, c: Color) {
    let (w, h) = (win.w, win.h);
    let stride = w as usize;
    let (vx, vy) = ((x1 - x0) as i64, (y1 - y0) as i64);
    let len2 = (vx * vx + vy * vy).max(1);
    let half_q4 = t as i64 * 8;
    let pad = t / 2 + 2;
    for y in (y0.min(y1) - pad).max(0)..(y0.max(y1) + pad + 1).min(h) {
        for x in (x0.min(x1) - pad).max(0)..(x0.max(x1) + pad + 1).min(w) {
            let (px, py) = ((x - x0) as i64, (y - y0) as i64);
            let s = ((px * vx + py * vy) * 256 / len2).clamp(0, 256);
            let (nx, ny) = (px * 256 - vx * s, py * 256 - vy * s);
            let d_q4 = isqrt((nx * nx + ny * ny) as u64) as i64 / 16;
            let a = edge(half_q4, d_q4);
            if a > 0 {
                let p = &mut win.pixels()[y as usize * stride + x as usize];
                *p = blend(*p, c, a as u8);
            }
        }
    }
}
