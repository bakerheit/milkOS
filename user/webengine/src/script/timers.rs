//! Timers (HTML §8.6 "Timers") and animation frame callbacks, generic over the callback
//! handle the script engine gives out. Times are milliseconds on the app's monotonic clock.
//!
//! - Delays are clamped to at least 0, and to 4 ms once timeouts nest more than 5 deep
//!   (a timer set from a timer callback), as the spec says.
//! - At most [`MAX_TIMERS`] timers at once (a page that sets more gets 0 back, nothing runs).
//! - Ties run in the order they were set.

use alloc::vec::Vec;

pub const MAX_TIMERS: usize = 10_000;
pub const MAX_FRAME_CALLBACKS: usize = 10_000;

#[derive(Clone, Debug)]
pub struct Timer<H> {
    pub id: u32,
    pub due: f64,
    /// setInterval: the period.
    pub interval: Option<f64>,
    pub cb: H,
    /// How deeply nested this timer's setting was.
    pub nesting: u32,
    seq: u64,
}

pub struct Timers<H> {
    list: Vec<Timer<H>>,
    next_id: u32,
    seq: u64,
    /// The nesting level of the timer callback running now (0 outside one).
    pub nesting: u32,
    frames: Vec<(u32, H)>,
    next_frame: u32,
}

impl<H: Clone> Default for Timers<H> {
    fn default() -> Self {
        Self::new()
    }
}

impl<H: Clone> Timers<H> {
    pub fn new() -> Timers<H> {
        Timers {
            list: Vec::new(),
            next_id: 1,
            seq: 0,
            nesting: 0,
            frames: Vec::new(),
            next_frame: 1,
        }
    }

    /// setTimeout / setInterval: the new id (0 if there are too many).
    pub fn add(&mut self, cb: H, delay: f64, repeat: bool, now: f64) -> u32 {
        if self.list.len() >= MAX_TIMERS {
            return 0;
        }
        let mut d = if delay.is_finite() && delay > 0.0 {
            delay.min(2_147_483_647.0)
        } else {
            0.0
        };
        let nesting = self.nesting + 1;
        if nesting > 5 && d < 4.0 {
            d = 4.0;
        }
        // An interval of 0 would spin: at least 1 ms between runs.
        if repeat && d < 1.0 {
            d = 1.0;
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.seq += 1;
        self.list.push(Timer {
            id,
            due: now + d,
            interval: repeat.then_some(d),
            cb,
            nesting,
            seq: self.seq,
        });
        id
    }

    /// clearTimeout / clearInterval (either clears either, as in browsers).
    pub fn clear(&mut self, id: u32) {
        self.list.retain(|t| t.id != id);
    }

    pub fn clear_all(&mut self) {
        self.list.clear();
        self.frames.clear();
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// When the next timer is due.
    pub fn next_due(&self) -> Option<f64> {
        self.list.iter().map(|t| t.due).reduce(f64::min)
    }

    /// The earliest timer due by `now`, taken off the list (an interval is put back for its
    /// next run, as from `now`). The caller runs it with [`Timers::nesting`] set to the
    /// timer's own nesting.
    pub fn take_due(&mut self, now: f64) -> Option<Timer<H>> {
        let mut best: Option<usize> = None;
        for (i, t) in self.list.iter().enumerate() {
            if t.due <= now
                && best.is_none_or(|b| {
                    let o = &self.list[b];
                    (t.due, t.seq) < (o.due, o.seq)
                })
            {
                best = Some(i);
            }
        }
        let i = best?;
        let t = self.list.remove(i);
        if let Some(iv) = t.interval {
            self.seq += 1;
            let mut again = t.clone();
            again.due = now + iv;
            again.seq = self.seq;
            self.list.push(again);
        }
        Some(t)
    }

    /// requestAnimationFrame: the new id (0 if too many are waiting).
    pub fn request_frame(&mut self, cb: H) -> u32 {
        if self.frames.len() >= MAX_FRAME_CALLBACKS {
            return 0;
        }
        let id = self.next_frame;
        self.next_frame = self.next_frame.wrapping_add(1).max(1);
        self.frames.push((id, cb));
        id
    }

    pub fn cancel_frame(&mut self, id: u32) {
        self.frames.retain(|f| f.0 != id);
    }

    /// Callbacks are waiting for the next frame.
    pub fn wants_frame(&self) -> bool {
        !self.frames.is_empty()
    }

    /// The callbacks for this frame (ones requested while they run wait for the next).
    pub fn take_frame(&mut self) -> Vec<(u32, H)> {
        core::mem::take(&mut self.frames)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_nesting_intervals() {
        let mut t: Timers<u32> = Timers::new();
        let a = t.add(1, 10.0, false, 0.0);
        let b = t.add(2, 5.0, false, 0.0);
        let c = t.add(3, 5.0, false, 0.0);
        assert!(a != b && b != c);
        assert_eq!(t.next_due(), Some(5.0));
        assert!(t.take_due(4.0).is_none());
        assert_eq!(t.take_due(20.0).unwrap().cb, 2);
        assert_eq!(t.take_due(20.0).unwrap().cb, 3);
        assert_eq!(t.take_due(20.0).unwrap().cb, 1);
        let i = t.add(9, 0.0, true, 100.0);
        assert!(t.take_due(100.0).is_none(), "an interval is at least 1 ms");
        assert_eq!(t.take_due(101.0).unwrap().cb, 9);
        assert_eq!(t.next_due(), Some(102.0));
        t.clear(i);
        assert!(t.is_empty());
        // Deeply nested timeouts are at least 4 ms.
        t.nesting = 6;
        t.add(1, 0.0, false, 0.0);
        assert_eq!(t.next_due(), Some(4.0));
        // NaN, negative and huge delays.
        t.nesting = 0;
        t.clear_all();
        t.add(1, f64::NAN, false, 0.0);
        t.add(1, -5.0, false, 0.0);
        t.add(1, 1e300, false, 0.0);
        assert_eq!(t.next_due(), Some(0.0));
    }

    #[test]
    fn frames_and_limits() {
        let mut t: Timers<u32> = Timers::new();
        let a = t.request_frame(1);
        t.request_frame(2);
        t.cancel_frame(a);
        assert!(t.wants_frame());
        let f = t.take_frame();
        assert_eq!(f.len(), 1);
        assert!(!t.wants_frame());
        for _ in 0..MAX_TIMERS {
            assert!(t.add(0, 1.0, false, 0.0) != 0);
        }
        assert_eq!(t.add(0, 1.0, false, 0.0), 0);
    }
}
