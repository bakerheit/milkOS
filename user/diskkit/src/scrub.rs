//! `/sys/fs/milkfs/scrub`: what it is doing, what it found, and the words to control it.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Never run this boot.
    Idle,
    /// Between volumes of one run.
    Starting,
    Running {
        run: u32,
        why: String,
        volume: String,
        done: u64,
        total: u64,
        errors: u32,
        /// Speed in tenths of MB/s.
        speed10: u32,
    },
    /// The last run's own summary line, and how many errors it found.
    Finished {
        run: u32,
        summary: String,
        errors: u32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScrubError {
    pub block: u64,
    pub what: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub phase: Phase,
    /// "32 MB/s" or "unlimited".
    pub rate: String,
    /// "every 7d", "off", ...
    pub auto: String,
    /// "never" or a time stamp.
    pub last_full: String,
    pub errors: Vec<ScrubError>,
}

impl Status {
    pub fn running(&self) -> bool {
        matches!(self.phase, Phase::Running { .. } | Phase::Starting)
    }

    /// Progress 0..=100 while running.
    pub fn percent(&self) -> Option<u32> {
        match &self.phase {
            Phase::Running { done, total, .. } => {
                Some((done * 100).checked_div(*total).unwrap_or(0).min(100) as u32)
            }
            _ => None,
        }
    }
}

fn errors_in(s: &str) -> u32 {
    // "... 2 error(s)" anywhere in the text.
    s.split("error(s)")
        .next()
        .and_then(|head| head.trim_end().rsplit(|c: char| !c.is_ascii_digit()).next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

fn speed10(s: &str) -> u32 {
    // "32.0 MB/s"
    s.split("MB/s")
        .next()
        .and_then(|h| h.trim_end().rsplit([' ', ',']).next())
        .map(|n| {
            let (i, f) = n.split_once('.').unwrap_or((n, "0"));
            i.parse::<u32>().unwrap_or(0) * 10
                + f.chars().next().and_then(|c| c.to_digit(10)).unwrap_or(0)
        })
        .unwrap_or(0)
}

/// Parse the text read from the control file.
pub fn parse(text: &str) -> Status {
    let mut st = Status {
        phase: Phase::Idle,
        rate: String::new(),
        auto: String::new(),
        last_full: String::new(),
        errors: Vec::new(),
    };
    for l in text.lines() {
        if let Some(rest) = l.strip_prefix("scrub: ") {
            if rest.starts_with("idle") {
                st.phase = Phase::Idle;
            } else if let Some(r) = rest.strip_prefix("[run ") {
                let (n, after) = r.split_once("] ").unwrap_or(("0", r));
                let run = n.parse().unwrap_or(0);
                if after.starts_with("running (next volume)") {
                    st.phase = Phase::Starting;
                } else if let Some(run_text) = after.strip_prefix("running (") {
                    // "running (requested) on /Volumes/Milk: 120/9907 blocks (1%), 32.0 MB/s, 0 error(s)"
                    let (why, rest) = run_text.split_once(") on ").unwrap_or(("", run_text));
                    let (volume, prog) = rest.split_once(": ").unwrap_or((rest, ""));
                    let (done, total) = prog
                        .split_whitespace()
                        .next()
                        .and_then(|p| p.split_once('/'))
                        .map(|(a, b)| (a.parse().unwrap_or(0), b.parse().unwrap_or(0)))
                        .unwrap_or((0, 0));
                    st.phase = Phase::Running {
                        run,
                        why: why.to_string(),
                        volume: volume.to_string(),
                        done,
                        total,
                        errors: errors_in(prog),
                        speed10: speed10(prog),
                    };
                } else {
                    st.phase = Phase::Finished {
                        run,
                        summary: after.to_string(),
                        errors: errors_in(after),
                    };
                }
            }
        } else if let Some(rest) = l.strip_prefix("rate: ") {
            // "32 MB/s, auto: every 7d, last full scrub of the primary volume: 2026-10-21T06:58:30"
            let (rate, tail) = rest.split_once(", auto: ").unwrap_or((rest, ""));
            st.rate = rate.to_string();
            let (auto, last) = tail
                .split_once(", last full scrub of the primary volume: ")
                .unwrap_or((tail, ""));
            st.auto = auto.to_string();
            st.last_full = last.to_string();
        } else if let Some(e) = l.strip_prefix("error: block ")
            && let Some((b, what)) = e.split_once(": ")
            && let Ok(block) = b.parse()
        {
            st.errors.push(ScrubError {
                block,
                what: what.to_string(),
            });
        }
    }
    st
}

/// How often the automatic scrub can run: (label, the control word).
pub const AUTO_CHOICES: [(&str, &str); 4] = [
    ("Off", "auto=off"),
    ("Daily", "auto=1d"),
    ("Weekly", "auto=7d"),
    ("Monthly", "auto=30d"),
];

/// The control word that sets the speed limit in MB/s (0 = unlimited).
pub fn rate_word(mb: u32) -> String {
    alloc::format!("rate={mb}")
}

/// Which of [`AUTO_CHOICES`] a status line says is on.
pub fn auto_choice(auto: &str) -> usize {
    match auto {
        a if a.contains("off") => 0,
        "every 1d" | "every 24h" => 1,
        "every 7d" => 2,
        "every 30d" => 3,
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle() {
        let s = parse(
            "scrub: idle (never run this boot)\nrate: 32 MB/s, auto: every 7d, last full scrub of the primary volume: never\n",
        );
        assert_eq!(s.phase, Phase::Idle);
        assert_eq!(
            (s.rate.as_str(), s.auto.as_str(), s.last_full.as_str()),
            ("32 MB/s", "every 7d", "never")
        );
        assert!(!s.running());
        assert_eq!(s.percent(), None);
    }

    #[test]
    fn running() {
        let s = parse(
            "scrub: [run 2] running (requested) on /Volumes/Milk: 4953/9907 blocks (49%), 31.6 MB/s, 1 error(s)\nrate: unlimited, auto: off, last full scrub of the primary volume: 2026-10-21T06:58:30\nerror: block 3: data of /Volumes/Milk/docs/b.bin at offset 0 (live, snapshot s1)\n",
        );
        assert_eq!(
            s.phase,
            Phase::Running {
                run: 2,
                why: "requested".into(),
                volume: "/Volumes/Milk".into(),
                done: 4953,
                total: 9907,
                errors: 1,
                speed10: 316
            }
        );
        assert!(s.running());
        assert_eq!(s.percent(), Some(49));
        assert_eq!(s.rate, "unlimited");
        assert_eq!(s.last_full, "2026-10-21T06:58:30");
        assert_eq!(
            s.errors,
            [ScrubError {
                block: 3,
                what: "data of /Volumes/Milk/docs/b.bin at offset 0 (live, snapshot s1)".into()
            }]
        );
    }

    #[test]
    fn finished_and_between_volumes() {
        let s = parse(
            "scrub: [run 1] done on /Volumes/Milk (requested): 9907 blocks (38 MiB) in 1228 ms, 32.0 MB/s, 2 error(s)\nrate: 32 MB/s, auto: every 7d, last full scrub of the primary volume: 2026-10-21T06:58:30\nerror: block 3: a\nerror: block 2: b\n",
        );
        match &s.phase {
            Phase::Finished {
                run,
                summary,
                errors,
            } => {
                assert_eq!((*run, *errors), (1, 2));
                assert!(summary.starts_with("done on /Volumes/Milk"));
            }
            p => panic!("{p:?}"),
        }
        assert_eq!(s.errors.len(), 2);
        let b = parse("scrub: [run 3] running (next volume)\n");
        assert_eq!(b.phase, Phase::Starting);
        assert!(b.running());
    }

    #[test]
    fn words() {
        assert_eq!(rate_word(8), "rate=8");
        assert_eq!(auto_choice("off"), 0);
        assert_eq!(auto_choice("every 7d"), 2);
        assert_eq!(auto_choice("every 30d"), 3);
        assert_eq!(AUTO_CHOICES[2].1, "auto=7d");
    }
}
