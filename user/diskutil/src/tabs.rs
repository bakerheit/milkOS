//! The five tabs, the page for other volumes, and what clicking their buttons does.

use crate::sheets::Sheet;
use crate::*;
use appkit::widgets::{chip, text_c};
use diskkit::scrub::{self, AUTO_CHOICES, Phase, RATES};
use diskkit::snaps::{self, ChangeKind, SnapKind, interval_words};
use gui::Color;

const X0: i32 = SIDE + 24;
const CW: i32 = W - SIDE - 48;
const DIFF_ROW: u32 = 5000;

fn card(win: &mut Window, th: &Theme, r: Rect, tint: Option<Color>) {
    match tint {
        Some(c) => widgets::bordered(win, r, 14, Theme::tint(th.milk, c, 0x22), c),
        None => widgets::bordered(win, r, 14, th.cream, th.sand),
    }
}

impl App {
    fn when_text(&self, secs: u64) -> String {
        let (d, sod) = timekit::date::split_secs(secs as i64);
        let (today, _) = timekit::date::split_secs(timeio::now_secs());
        let day = if d == today {
            String::from("Today")
        } else {
            d.fmt()
        };
        format!("{day} {:02}:{:02}", sod / 3600, sod / 60 % 60)
    }

    pub fn draw_overview(&mut self, win: &mut Window) {
        let th = self.th;
        let Some(info) = self.disk.info.clone() else {
            return;
        };
        bold_x(win, X0, BAR + 14, &info.label, th.espresso, 2);
        let enc = match info.encrypted {
            Some(true) => ", encrypted and unlocked",
            Some(false) => ", not encrypted",
            None => "",
        };
        win.text(
            X0,
            BAR + 52,
            &format!("milkfs, {}{enc}", diskkit::fmt::kb(info.total_kb())),
            th.cocoa,
        );
        // The usage bar.
        let bar = Rect::new(X0, BAR + 80, CW, 24);
        win.rrect(bar.x, bar.y, bar.w, bar.h, 12, th.oat);
        let used_w = (bar.w as i64 * info.used_pct10() as i64 / 1000) as i32;
        if used_w > 0 {
            win.rrect(bar.x, bar.y, used_w.max(bar.h.min(24)), bar.h, 12, th.honey);
        }
        win.text(
            X0,
            BAR + 112,
            &format!(
                "{} used   {} free",
                diskkit::fmt::kb(info.used_kb()),
                diskkit::fmt::kb(info.free_kb())
            ),
            th.espresso,
        );
        // Four fact cards.
        let last = self.disk.scrub.as_ref().map_or("never", |s| {
            if s.last_full.is_empty() {
                "never"
            } else {
                s.last_full.as_str()
            }
        });
        let facts = [
            ("Snapshots", format!("{}", self.disk.snaps.len())),
            ("Clones", format!("{}", self.disk.clones.len())),
            ("Generation", format!("{}", info.generation)),
            (
                "Last scrub",
                String::from(last.split('T').next().unwrap_or(last)),
            ),
        ];
        let cw = (CW - 3 * 12) / 4;
        for (i, (k, v)) in facts.iter().enumerate() {
            let r = Rect::new(X0 + i as i32 * (cw + 12), BAR + 142, cw, 64);
            card(win, &th, r, None);
            win.text(r.x + 14, r.y + 10, k, th.cocoa);
            bold_x(
                win,
                r.x + 14,
                r.y + 34,
                &widgets::fit(v, cw - 28),
                th.espresso,
                1,
            );
        }
        // The Time Machine card.
        let tm = Rect::new(X0, BAR + 222, CW, 112);
        card(win, &th, tm, None);
        bold_x(win, tm.x + 16, tm.y + 12, "Time Machine", th.espresso, 1);
        match self.disk.auto.clone() {
            Some(a) => {
                let line = if a.enabled {
                    format!(
                        "On   every {}   keeps the last {} and one a day for {} days",
                        interval_words(&a.interval),
                        a.keep,
                        a.daily
                    )
                } else {
                    String::from("Off")
                };
                win.text(
                    tm.x + 16,
                    tm.y + 36,
                    &widgets::fit(&line, tm.w - 32),
                    th.espresso,
                );
                let next = match (&a.next_in, a.enabled) {
                    (Some(n), true) => format!("Next snapshot in {}", snaps::eta_words(n)),
                    _ => format!("{} automatic snapshots so far", a.count),
                };
                win.text(tm.x + 16, tm.y + 58, &next, th.cocoa);
            }
            None => {
                win.text(
                    tm.x + 16,
                    tm.y + 36,
                    "Time Machine is not available",
                    th.cocoa,
                );
            }
        }
        let sw = button_w("Snapshot now", true);
        button(
            win,
            &th,
            &mut self.hits,
            Rect::new(tm.x + 16, tm.y + 80, sw, 24),
            "Snapshot now",
            Kind::Primary,
            TM_NOW,
            self.job.is_none(),
            true,
        );
        let stw = button_w("Settings", true);
        button(
            win,
            &th,
            &mut self.hits,
            Rect::new(tm.x + 16 + sw + 8, tm.y + 80, stw, 24),
            "Settings",
            Kind::Secondary,
            TM_SETTINGS,
            true,
            true,
        );
        // The health card.
        let bad = self
            .disk
            .scrub
            .as_ref()
            .is_some_and(|s| !s.errors.is_empty());
        let hc = Rect::new(X0, BAR + 350, CW, 56);
        widgets::bordered(
            win,
            hc,
            14,
            Theme::tint(th.milk, if bad { th.danger } else { th.matcha }, 0x22),
            if bad { th.danger } else { th.matcha },
        );
        if bad {
            bold_x(
                win,
                hc.x + 16,
                hc.y + 10,
                "The last scrub found problems",
                th.danger,
                1,
            );
            win.text(
                hc.x + 16,
                hc.y + 32,
                "Open Health to see the files",
                th.cocoa,
            );
        } else {
            bold_x(
                win,
                hc.x + 16,
                hc.y + 10,
                "No problems found",
                th.espresso,
                1,
            );
            win.text(
                hc.x + 16,
                hc.y + 32,
                if info.healthy() {
                    "The volume is healthy"
                } else {
                    "The volume reports a fault"
                },
                th.cocoa,
            );
        }
        // The records meter.
        let used = self.disk.records_used();
        let mut y = BAR + 422;
        if used >= 24 {
            win.text(
                X0,
                y,
                &format!(
                    "{used} of {} snapshot and clone records used",
                    diskkit::names::MAX_SNAPSHOTS
                ),
                th.espresso,
            );
            charts_bar(
                win,
                Rect::new(X0, y + 22, CW, 12),
                (used * 1000 / diskkit::names::MAX_SNAPSHOTS) as u32,
                th.honey,
                th.oat,
            );
            win.text(
                X0,
                y + 40,
                "The oldest automatic snapshot makes room; manual snapshots are never deleted.",
                th.cocoa,
            );
            y += 66;
        }
        if info.encrypted == Some(true) {
            win.text(X0, y, "Backups made with Send are not encrypted.", th.cocoa);
        }
    }

    pub fn draw_snapshots(&mut self, win: &mut Window) {
        let th = self.th;
        let snaps = self.disk.snaps.clone();
        // Header and rows.
        let top = BAR + 12;
        win.fill(X0, top, CW, 28, th.cream);
        widgets::hline(win, X0, top + 27, CW, th.sand);
        for (x, l) in [
            (X0 + 12, "Name"),
            (X0 + 300, "Created"),
            (X0 + 460, "Kind"),
            (X0 + 590, "Clones"),
        ] {
            bold_x(win, x, top + 6, l, th.cocoa, 1);
        }
        let rows_h = 6 * 30;
        if snaps.is_empty() {
            text_c(
                win,
                X0 + CW / 2,
                top + 80,
                "No snapshots yet. Take one to keep this moment.",
                th.cocoa,
                1,
            );
        }
        let max_scroll = (snaps.len() as i32 * 30 - rows_h).max(0);
        self.scroll = self.scroll.clamp(0, max_scroll) / 30 * 30;
        for (i, s) in snaps
            .iter()
            .enumerate()
            .skip((self.scroll / 30) as usize)
            .take(6)
        {
            let y = top + 28 + (i - (self.scroll / 30) as usize) as i32 * 30;
            let sel = self.sel_snap.as_deref() == Some(s.name.as_str());
            if sel {
                win.fill(X0, y, CW, 30, Theme::tint(th.milk, th.straw, 0x33));
            }
            win.text(X0 + 12, y + 7, &widgets::fit(&s.name, 306), th.espresso);
            win.text(X0 + 300, y + 7, &s.when(), th.espresso);
            let kind = match s.kind {
                SnapKind::Auto => "Auto",
                SnapKind::Before => "Before install",
                SnapKind::Manual => "Manual",
            };
            win.text(X0 + 460, y + 7, kind, th.espresso);
            let n = snaps::clones_of(&self.disk.clones, &s.name).len();
            win.text(X0 + 590, y + 7, &format!("{n}"), th.espresso);
            widgets::hline(win, X0, y + 29, CW, th.sand);
            self.hits.add(Rect::new(X0, y, CW, 30), SNAP_ROW + i as u32);
        }
        // The action bar.
        let by = top + 28 + rows_h + 10;
        let sel = self
            .sel_snap
            .clone()
            .filter(|n| snaps.iter().any(|s| &s.name == n));
        let has = sel.is_some();
        let has_clone = sel.as_deref().is_some_and(|n| self.disk.snap_has_clone(n));
        let idle = self.job.is_none();
        let mut x = X0;
        for (label, id, kind, on) in [
            ("Diff", B_DIFF, Kind::Primary, has),
            ("Clone...", B_CLONE, Kind::Secondary, has),
            ("Roll back...", B_ROLLBACK, Kind::Secondary, has),
            ("Send...", B_SEND, Kind::Secondary, has && idle),
        ] {
            let w = button_w(label, true);
            button(
                win,
                &th,
                &mut self.hits,
                Rect::new(x, by, w, 24),
                label,
                kind,
                id,
                on,
                true,
            );
            x += w + 8;
        }
        let dw = button_w("Delete...", true);
        button(
            win,
            &th,
            &mut self.hits,
            Rect::new(X0 + CW - dw, by, dw, 24),
            "Delete...",
            Kind::Secondary,
            B_DELETE,
            has && !has_clone,
            true,
        );
        let rw = button_w("Restore from file...", true);
        button(
            win,
            &th,
            &mut self.hits,
            Rect::new(X0, by + 32, rw, 24),
            "Restore from file...",
            Kind::Secondary,
            B_RESTORE,
            idle,
            true,
        );
        if has_clone {
            win.text(
                X0 + rw + 16,
                by + 36,
                "This snapshot has a clone; discard the clone first.",
                th.cocoa,
            );
        }
        // The diff.
        if let Some((head, d)) = self.diff.clone() {
            let dy = by + 70;
            bold_x(win, X0, dy, &widgets::fit(&head, CW), th.espresso, 1);
            if d.changes.is_empty() {
                win.text(X0, dy + 30, "No changes.", th.cocoa);
            }
            for (i, c) in d
                .changes
                .iter()
                .enumerate()
                .take(((H - 24 - dy - 30) / 28).max(0) as usize)
            {
                let y = dy + 28 + i as i32 * 28;
                let (letter, col) = match c.kind {
                    ChangeKind::Added => ("A", th.matcha),
                    ChangeKind::Modified => ("M", th.honey),
                    ChangeKind::Removed => ("D", th.danger),
                    ChangeKind::Renamed => ("R", th.blue),
                };
                win.rrect(X0, y, 26, 22, 6, Theme::tint(th.milk, col, 0x44));
                bold_x(win, X0 + 8, y + 3, letter, th.ink, 1);
                let path = match &c.to {
                    Some(t) => format!("{} -> {}", c.path, t),
                    None => c.path.clone(),
                };
                win.text(X0 + 36, y + 3, &widgets::fit(&path, CW - 40), th.espresso);
                self.hits.add(Rect::new(X0, y, CW, 24), DIFF_ROW + i as u32);
            }
        }
    }

    pub fn draw_clones(&mut self, win: &mut Window) {
        let th = self.th;
        let clones = self.disk.clones.clone();
        let label = self.volume_label();
        if clones.is_empty() {
            text_c(win, X0 + CW / 2, BAR + 120, "No clones.", th.cocoa, 1);
            text_c(
                win,
                X0 + CW / 2,
                BAR + 146,
                "A clone is a writable copy of a snapshot. Try something in it, then keep or discard it.",
                th.cocoa,
                1,
            );
        }
        for (i, c) in clones.iter().enumerate() {
            let y = BAR + 14 + i as i32 * 116;
            if y + 100 > H - 30 {
                break;
            }
            let r = Rect::new(X0, y, CW, 104);
            card(win, &th, r, None);
            win.rrect(r.x + 14, r.y + 14, 40, 40, 10, th.lav);
            bold_x(win, r.x + 68, r.y + 12, &c.name, th.espresso, 1);
            let made = self
                .disk
                .snaps
                .iter()
                .find(|s| s.name == c.origin)
                .map_or(String::new(), |s| {
                    format!(", made {}", self.when_text(s.created))
                });
            win.text(
                r.x + 68,
                r.y + 32,
                &widgets::fit(&format!("from the snapshot {}{}", c.origin, made), r.w - 84),
                th.cocoa,
            );
            let opens = c
                .mount
                .clone()
                .unwrap_or_else(|| format!("/Volumes/{label}-{}", c.name));
            win.text(
                r.x + 68,
                r.y + 50,
                &widgets::fit(&format!("Opens as {opens}"), r.w - 84),
                th.cocoa,
            );
            let ow = button_w("Open in Files", true);
            let kw = button_w("Keep...", true);
            let dw = button_w("Discard...", true);
            let by = r.y + 72;
            button(
                win,
                &th,
                &mut self.hits,
                Rect::new(r.x + 68, by, ow, 24),
                "Open in Files",
                Kind::Secondary,
                C_OPEN + i as u32,
                c.mount.is_some(),
                true,
            );
            button(
                win,
                &th,
                &mut self.hits,
                Rect::new(r.x + 68 + ow + 8, by, kw, 24),
                "Keep...",
                Kind::Primary,
                C_KEEP + i as u32,
                true,
                true,
            );
            button(
                win,
                &th,
                &mut self.hits,
                Rect::new(r.x + 68 + ow + kw + 16, by, dw, 24),
                "Discard...",
                Kind::Secondary,
                C_DISCARD + i as u32,
                true,
                true,
            );
        }
        win.text(
            X0,
            H - 24 - 40,
            "Keep makes the clone live; the current state is kept as before-<name>.",
            th.cocoa,
        );
        win.text(
            X0,
            H - 24 - 22,
            "Discard frees what the clone wrote. Neither can be undone.",
            th.cocoa,
        );
    }

    pub fn draw_health(&mut self, win: &mut Window) {
        let th = self.th;
        let st = self.disk.scrub.clone().unwrap_or_else(|| scrub::parse(""));
        // The scrub card.
        let sc = Rect::new(X0, BAR + 14, CW, 132);
        card(win, &th, sc, None);
        bold_x(win, sc.x + 16, sc.y + 12, "Scrub", th.espresso, 1);
        match &st.phase {
            Phase::Running {
                why,
                done,
                total,
                speed10,
                errors,
                ..
            } => {
                win.text(
                    sc.x + 16,
                    sc.y + 36,
                    &format!("Running ({why})   {errors} error(s) so far"),
                    th.espresso,
                );
                let pct = st.percent().unwrap_or(0);
                charts_bar(
                    win,
                    Rect::new(sc.x + 16, sc.y + 62, sc.w - 32 - 80, 14),
                    pct * 10,
                    th.straw,
                    th.oat,
                );
                win.text(
                    sc.x + 16,
                    sc.y + 84,
                    &format!(
                        "{pct}%   {} of {} read   {}.{} MB/s",
                        diskkit::fmt::kb(done * 4),
                        diskkit::fmt::kb(total * 4),
                        speed10 / 10,
                        speed10 % 10
                    ),
                    th.cocoa,
                );
                let w = button_w("Stop", true);
                button(
                    win,
                    &th,
                    &mut self.hits,
                    Rect::new(sc.right() - 16 - w, sc.y + 58, w, 24),
                    "Stop",
                    Kind::Danger,
                    SCRUB_STOP,
                    true,
                    true,
                );
            }
            Phase::Starting => {
                win.text(
                    sc.x + 16,
                    sc.y + 36,
                    "Starting the next volume...",
                    th.espresso,
                );
            }
            Phase::Finished { summary, .. } => {
                // "done on /Volumes/Milk (requested): 532 blocks (2 MiB) in 65 ms, 31.5 MB/s, 0 error(s)"
                let parts: Vec<&str> = summary.splitn(2, ": ").collect();
                win.text(
                    sc.x + 16,
                    sc.y + 36,
                    &widgets::fit(parts[0], sc.w - 32),
                    th.espresso,
                );
                if let Some(rest) = parts.get(1) {
                    win.text(
                        sc.x + 16,
                        sc.y + 58,
                        &widgets::fit(rest, sc.w - 32),
                        th.cocoa,
                    );
                }
            }
            Phase::Idle => {
                win.text(
                    sc.x + 16,
                    sc.y + 36,
                    "Not run since the machine started.",
                    th.cocoa,
                );
            }
        }
        win.text(sc.x + 16, sc.y + 106, "Speed", th.cocoa);
        let cur = RATES
            .iter()
            .position(|r| st.rate.starts_with(&format!("{r} ")));
        let mut x = sc.x + 72;
        for (i, r) in RATES.iter().enumerate() {
            x += chip(
                win,
                &th,
                &mut self.hits,
                x,
                sc.y + 102,
                &format!("{r} MB/s"),
                cur == Some(i),
                RATE + i as u32,
            ) + 6;
        }
        // The schedule card.
        let hc = Rect::new(X0, BAR + 158, CW, 86);
        card(win, &th, hc, None);
        bold_x(
            win,
            hc.x + 16,
            hc.y + 12,
            "Scrub automatically every",
            th.espresso,
            1,
        );
        let pick = scrub::auto_choice(&st.auto);
        let mut x = hc.x + 16;
        for (i, (l, _)) in AUTO_CHOICES.iter().enumerate() {
            x += chip(
                win,
                &th,
                &mut self.hits,
                x,
                hc.y + 36,
                l,
                pick == Some(i),
                AUTO + i as u32,
            ) + 6;
        }
        let last = if st.last_full.is_empty() || st.last_full == "never" {
            String::from("never")
        } else {
            st.last_full.replace('T', " ")
        };
        let ok = st.errors.is_empty();
        win.text(
            hc.x + 16,
            hc.y + 62,
            &format!(
                "Last full scrub: {last}{}",
                if ok { ", no problems found" } else { "" }
            ),
            th.cocoa,
        );
        // Errors.
        let mut y = BAR + 258;
        if !st.errors.is_empty() {
            bold_x(win, X0, y, "The scrub found damage:", th.danger, 1);
            y += 24;
            for e in st.errors.iter().take(4) {
                win.text(
                    X0,
                    y,
                    &widgets::fit(&format!("block {}: {}", e.block, e.what), CW),
                    th.danger,
                );
                y += 20;
            }
            win.text(
                X0,
                y,
                "Restore the file from an older snapshot (Versions in Files).",
                th.cocoa,
            );
            y += 30;
        }
        // The structure check card.
        let cc = Rect::new(X0, y, CW, H - 24 - y - 10);
        if cc.h > 60 {
            card(win, &th, cc, None);
            bold_x(win, cc.x + 16, cc.y + 12, "Structure check", th.espresso, 1);
            let w = button_w("Check", true);
            button(
                win,
                &th,
                &mut self.hits,
                Rect::new(cc.x + 170, cc.y + 8, w, 24),
                "Check",
                Kind::Secondary,
                CHECK,
                self.job.is_none(),
                true,
            );
            let rw = button_w("Repair...", true);
            button(
                win,
                &th,
                &mut self.hits,
                Rect::new(cc.x + 170 + w + 8, cc.y + 8, rw, 24),
                "Repair...",
                Kind::Secondary,
                REPAIR,
                self.job.is_none(),
                true,
            );
            let lines: Vec<&str> = self
                .check_report
                .lines()
                .take(((cc.h - 48) / 18).max(0) as usize)
                .collect();
            if lines.is_empty() {
                win.text(
                    cc.x + 16,
                    cc.y + 44,
                    "Check changes nothing; Repair fixes what is safe to fix.",
                    th.cocoa,
                );
            }
            for (i, l) in lines.iter().enumerate() {
                win.text(
                    cc.x + 16,
                    cc.y + 44 + i as i32 * 18,
                    &widgets::fit(l, cc.w - 32),
                    th.espresso,
                );
            }
        }
    }

    pub fn draw_security(&mut self, win: &mut Window) {
        let th = self.th;
        let info = self.disk.info.clone();
        let enc = info.as_ref().and_then(|i| i.encrypted);
        let ec = Rect::new(X0, BAR + 14, CW, 126);
        let (head, tint) = match enc {
            Some(true) => ("Encrypted and unlocked", Some(th.matcha)),
            Some(false) => ("Not encrypted", None),
            None => ("Encryption status is not reported", None),
        };
        card(win, &th, ec, tint);
        bold_x(win, ec.x + 16, ec.y + 12, head, th.espresso, 1);
        let sentence = match enc {
            Some(true) => {
                "Every block is encrypted with a key that only your passphrase or the recovery key can open."
            }
            Some(false) => "Encryption is set up when the disk is made on the Mac.",
            None => "The system did not say whether this volume is encrypted.",
        };
        win.text(
            ec.x + 16,
            ec.y + 34,
            &widgets::fit(sentence, ec.w - 32),
            th.cocoa,
        );
        // The details the kernel reports: cipher, key derivation, key slots.
        if let Some(i) = info.as_ref().filter(|i| i.encrypted == Some(true)) {
            let d = format!("{}   {}   {}", i.kdf, i.key_wrap, i.key_slots);
            win.text(ec.x + 16, ec.y + 54, &widgets::fit(&d, ec.w - 32), th.cocoa);
        }
        let cw = button_w("Change passphrase...", true);
        button(
            win,
            &th,
            &mut self.hits,
            Rect::new(ec.x + 16, ec.y + 76, cw, 24),
            "Change passphrase...",
            Kind::Secondary,
            CHG_PASS,
            enc != Some(false),
            true,
        );
        let primary_path = format!("/Volumes/{}", self.volume_label());
        let entry = self.disk.lock_entry(&primary_path).cloned();
        let (can_lock, lock_note) = match (&entry, enc) {
            (Some(e), _) if e.home => (
                false,
                "You can't lock the volume you're logged into; it locks at shutdown.",
            ),
            (Some(_), _) if self.job.is_some() => (false, "Wait for the running job to finish."),
            (Some(_), _) => (
                true,
                "Locking closes the volume and wipes the key from memory.",
            ),
            (None, Some(false)) => (false, "Only an encrypted volume can be locked."),
            (None, _) => (false, "The system lists no encrypted volume to lock."),
        };
        let lw = button_w("Lock now", true);
        button(
            win,
            &th,
            &mut self.hits,
            Rect::new(ec.x + 16 + cw + 8, ec.y + 76, lw, 24),
            "Lock now",
            Kind::Secondary,
            LOCK,
            can_lock,
            true,
        );
        win.text(
            ec.x + 16 + cw + lw + 24,
            ec.y + 80,
            &widgets::fit(lock_note, ec.w - cw - lw - 56),
            th.cocoa,
        );
        // The recovery key card.
        let rc = Rect::new(X0, BAR + 152, CW, 64);
        card(win, &th, rc, None);
        bold_x(win, rc.x + 16, rc.y + 10, "Recovery key", th.espresso, 1);
        win.text(
            rc.x + 16,
            rc.y + 34,
            "It was shown once when the disk was made and can not be shown again.",
            th.cocoa,
        );
        // The backups warning.
        let bc = Rect::new(X0, BAR + 228, CW, 112);
        widgets::bordered(win, bc, 14, Theme::tint(th.milk, th.honey, 0x22), th.honey);
        bold_x(
            win,
            bc.x + 16,
            bc.y + 10,
            "Backups are not encrypted",
            th.espresso,
            1,
        );
        win.text(
            bc.x + 16,
            bc.y + 34,
            "A backup made with Send is a plain copy of your files.",
            th.espresso,
        );
        win.text(
            bc.x + 16,
            bc.y + 52,
            "Keep it on an encrypted disk, or receive it into another encrypted volume.",
            th.espresso,
        );
        let bw = button_w("Back up...", true);
        button(
            win,
            &th,
            &mut self.hits,
            Rect::new(bc.x + 16, bc.y + 80, bw, 24),
            "Back up...",
            Kind::Primary,
            BACKUP,
            self.job.is_none() && !self.disk.snaps.is_empty(),
            true,
        );
        win.text(
            X0,
            BAR + 356,
            "Creating or erasing a volume is not offered here: the disk is made on the Mac.",
            th.cocoa,
        );
    }

    /// The page for any row but the primary volume (and for a locked one).
    pub fn draw_other(&mut self, win: &mut Window) {
        let th = self.th;
        let rows = self.rows();
        let Some(row) = rows.get(self.vol).cloned_row() else {
            return;
        };
        bold_x(win, X0, BAR + 14, &row.name, th.espresso, 2);
        win.text(X0, BAR + 52, &row.kind, th.cocoa);
        if row.locked {
            let c = Rect::new(X0, BAR + 90, CW, 100);
            card(win, &th, c, None);
            widgets::padlock(win, c.x + 18, c.y + 16, th.espresso);
            bold_x(
                win,
                c.x + 44,
                c.y + 14,
                &format!("{} is encrypted and locked", row.name),
                th.espresso,
                1,
            );
            win.text(
                c.x + 16,
                c.y + 44,
                "Unlock it with its passphrase or the recovery key to use it.",
                th.cocoa,
            );
            let w = button_w("Unlock...", true);
            button(
                win,
                &th,
                &mut self.hits,
                Rect::new(c.x + 16, c.y + 66, w, 24),
                "Unlock...",
                Kind::Primary,
                UNLOCK,
                true,
                true,
            );
            return;
        }
        let lines: &[&str] = match row.icon {
            Icon::Share => &["The folder shared with the Mac."],
            Icon::Ram => &[
                "The root of the file system lives in memory and is not kept across a restart.",
                "Your home folder and apps are on the Milk disk.",
            ],
            Icon::Clone => &[
                "A clone of a snapshot: a writable copy that shares the disk's blocks.",
                "Manage it on the Clones tab of the Milk disk.",
            ],
            Icon::Disk => &[],
        };
        for (i, l) in lines.iter().enumerate() {
            win.text(X0, BAR + 130 + i as i32 * 22, l, th.espresso);
        }
        // Size and free space from statfs.
        let usage = row
            .path
            .as_deref()
            .and_then(|p| self.disk.usage_of(p))
            .copied();
        if let Some(u) = usage {
            let bar = Rect::new(X0, BAR + 80, CW, 20);
            charts_bar(win, bar, u.used_pct10(), th.honey, th.oat);
            win.text(
                X0,
                BAR + 106,
                &format!(
                    "{} used   {} free   {} in all{}",
                    diskkit::fmt::kb(u.total_kb.saturating_sub(u.free_kb)),
                    diskkit::fmt::kb(u.free_kb),
                    diskkit::fmt::kb(u.total_kb),
                    if u.read_only { "   read-only" } else { "" }
                ),
                th.espresso,
            );
        } else {
            win.text(
                X0,
                BAR + 84,
                "The system reports no size for this volume.",
                th.cocoa,
            );
        }
        if row.icon == Icon::Share {
            let rw = match (self.disk.share_rw, usage.map(|u| u.read_only)) {
                (Some(true), _) => "The Mac folder can be written to (backups are possible).",
                (_, Some(true)) | (Some(false), _) => {
                    "It is read-only unless milkOS was started with MILK_SHARE_RW=1."
                }
                _ => "",
            };
            win.text(X0, BAR + 130 + lines.len() as i32 * 22, rw, th.cocoa);
        }
        // An encrypted volume that is mounted can be locked again.
        if let Some(e) = row
            .path
            .as_deref()
            .and_then(|p| self.disk.lock_entry(p))
            .cloned()
        {
            let (ok, note) = if e.home {
                (
                    false,
                    "You can't lock the volume you're logged into; it locks at shutdown.",
                )
            } else {
                (
                    self.job.is_none(),
                    "Locking closes the volume and wipes the key from memory.",
                )
            };
            let w = button_w("Lock now", true);
            button(
                win,
                &th,
                &mut self.hits,
                Rect::new(X0, BAR + 170, w, 24),
                "Lock now",
                Kind::Secondary,
                LOCK_OTHER,
                ok,
                true,
            );
            win.text(X0 + w + 12, BAR + 174, note, th.cocoa);
        }
    }

    pub fn tab_click(&mut self, id: u32) {
        match id {
            TM_NOW => {
                if let Err(e) = data::ctl("auto", "now") {
                    self.fail("Could not take the snapshot", e);
                }
            }
            TM_SETTINGS => self.open_tm_settings(),
            B_DIFF => self.run_diff(),
            B_CLONE => self.open_new_clone(),
            B_ROLLBACK => self.ask_rollback(),
            B_DELETE => self.ask_delete(),
            B_SEND | BACKUP => self.open_backup(),
            B_RESTORE => self.open_restore(),
            SCRUB_STOP => {
                let _ = data::ctl("scrub", "stop");
            }
            r if (RATE..RATE + 3).contains(&r) => {
                let _ = data::ctl("scrub", &scrub::rate_word(RATES[(r - RATE) as usize]));
            }
            a if (AUTO..AUTO + 3).contains(&a) => {
                if let Err(e) = data::ctl("scrub", AUTO_CHOICES[(a - AUTO) as usize].1) {
                    self.fail("Could not change the schedule", e);
                }
            }
            CHECK => {
                self.check_report = String::new();
                self.job =
                    data::spawn_write("Checking the volume", "check", "check", timeio::mono_ms());
            }
            REPAIR => self.ask_repair(),
            CHG_PASS => self.open_passwd(),
            LOCK => {
                let path = format!("/Volumes/{}", self.volume_label());
                self.lock_volume(&path);
            }
            LOCK_OTHER => {
                let path = self
                    .rows()
                    .get(self.vol)
                    .and_then(|r| r.path.clone())
                    .unwrap_or_default();
                self.lock_volume(&path);
            }
            UNLOCK => self.open_unlock(),
            c if (C_OPEN..C_OPEN + 100).contains(&c) => {
                if let Some(m) = self
                    .disk
                    .clones
                    .get((c - C_OPEN) as usize)
                    .and_then(|c| c.mount.clone())
                {
                    self.open_in_files(&m);
                }
            }
            c if (C_KEEP..C_KEEP + 100).contains(&c) => self.ask_keep((c - C_KEEP) as usize),
            c if (C_DISCARD..C_DISCARD + 100).contains(&c) => {
                self.ask_discard((c - C_DISCARD) as usize)
            }
            d if (DIFF_ROW..DIFF_ROW + 500).contains(&d) => {
                if let Some((_, diff)) = &self.diff
                    && let Some(c) = diff.changes.get((d - DIFF_ROW) as usize)
                {
                    let path = c.to.clone().unwrap_or_else(|| c.path.clone());
                    let parent = path
                        .rsplit_once('/')
                        .map_or("/", |(p, _)| if p.is_empty() { "/" } else { p });
                    self.open_in_files(&String::from(parent));
                }
            }
            _ => {}
        }
    }

    fn run_diff(&mut self) {
        let Some(name) = self.sel_snap.clone() else {
            return;
        };
        match data::ctl("diff", &snaps::diff_request(&name, None)) {
            Ok(()) => {
                let d = snaps::parse_diff(&data::text("/sys/fs/milkfs/diff"));
                let head = format!(
                    "{name} compared with the live volume: {} change{}",
                    d.changes.len(),
                    if d.changes.len() == 1 { "" } else { "s" }
                );
                self.diff = Some((head, d));
            }
            Err(e) => self.fail("Could not compare", e),
        }
    }

    pub fn open_in_files(&self, path: &str) {
        if let Ok(fd) = rt::sys::open("/dev/wm", rt::sys::O_WRONLY | rt::sys::O_CLOEXEC) {
            let _ = rt::sys::write_all(fd, format!("open {path}\n").as_bytes());
            let _ = rt::sys::close(fd);
        }
    }

    pub fn take_snapshot(&mut self) {
        let (d, sod) = timekit::date::split_secs(timeio::now_secs());
        let name = diskkit::names::suggest(d, (sod / 60) as u16);
        if let Err(e) = data::ctl("snapshot", &name) {
            self.fail("Could not take the snapshot", e);
        } else {
            self.sel_snap = Some(name);
            self.disk.refresh();
        }
    }

    /// Lock an encrypted volume by its mount path. A refusal names who is using it.
    fn lock_volume(&mut self, path: &str) {
        let Some(device) = self.disk.lock_entry(path).map(|e| e.device.clone()) else {
            return;
        };
        match data::ctl("lock", &diskkit::fsinfo::lock_request(&device)) {
            Ok(()) => {
                self.vol = 0;
                self.disk.refresh();
            }
            Err(e) => self.fail("The volume can not be locked", e),
        }
    }

    /// Show why a control write failed: the kernel's own reason from `/sys/fs/milkfs/error` when
    /// it has one (with the processes holding a volume when a lock is refused), else our words.
    pub fn fail(&mut self, title: &str, errno: i32) {
        let mut body: Vec<String> = Vec::new();
        let mut details = format!("Details: error {errno}");
        match data::last_error() {
            Some(e) => {
                let holders = diskkit::fsinfo::holders(&e.reason);
                if holders.is_empty() {
                    body.push(sentence(&e.reason));
                } else {
                    body.push(String::from("It is in use. Close these first:"));
                    body.extend(holders.into_iter().take(4).map(|h| format!("  {h}")));
                }
                details = format!("Details: {} {} (error {errno})", e.control, e.errno_name);
            }
            None if errno == 13 => body.push(String::from(
                "This needs authorisation, and the system cannot ask for it yet.",
            )),
            None => body.push(String::from(diskkit::names::why(errno))),
        }
        body.push(details);
        self.sheet = Some(Sheet::Message {
            title: String::from(title),
            body,
            danger: true,
        });
    }
}

/// "the volume is busy" as a sentence: first letter up, a full stop.
fn sentence(s: &str) -> String {
    let mut c = s.chars();
    let mut out: String = c
        .next()
        .map(|f| f.to_uppercase().collect())
        .unwrap_or_default();
    out.push_str(c.as_str());
    if !out.ends_with('.') {
        out.push('.');
    }
    out
}

fn charts_bar(win: &mut Window, r: Rect, tenths: u32, fill: Color, track: Color) {
    win.rrect(r.x, r.y, r.w, r.h, r.h / 2, track);
    let w = (r.w as i64 * tenths.min(1000) as i64 / 1000) as i32;
    if w > 0 {
        win.rrect(r.x, r.y, w.max(r.h), r.h, r.h / 2, fill);
    }
}

trait ClonedRow {
    fn cloned_row(self) -> Option<Row>;
}

impl ClonedRow for Option<&Row> {
    fn cloned_row(self) -> Option<Row> {
        self.map(|r| Row {
            name: r.name.clone(),
            kind: r.kind.clone(),
            icon: r.icon,
            locked: r.locked,
            path: r.path.clone(),
        })
    }
}
