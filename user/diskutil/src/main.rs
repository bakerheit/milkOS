//! Disk Utility: a window over milkfs (docs/ui-concepts/SPEC-DISK-REMINDERS.md, section 1).
//! Everything comes from /System/Kernel/fs/milkfs and /proc/mounts; the logic is in `user/diskkit`.
//! Destructive actions go through one confirm sheet.

#![no_std]
#![no_main]

extern crate alloc;

mod data;
mod sheets;
mod tabs;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use appkit::widgets::{self, Kind, bold_x, button, button_w, segmented};
use appkit::{Hits, Rect, Theme};
use data::{Disk, Job};
use diskkit::mounts::Kind as VolKind;
use diskkit::snaps::Diff;
use gui::{Event, Key, Window, blend};
use sheets::Sheet;

rt::main!(main);

pub const W: i32 = 900;
pub const H: i32 = 620;
pub const SIDE: i32 = 188;
pub const BAR: i32 = 44;

// Hit ids.
pub const TAB: u32 = 10; // 10..14
pub const PRIMARY: u32 = 20;
pub const VOL: u32 = 100; // + sidebar row
pub const SNAP_ROW: u32 = 1000; // + snapshot index
pub const B_DIFF: u32 = 30;
pub const B_CLONE: u32 = 31;
pub const B_ROLLBACK: u32 = 32;
pub const B_SEND: u32 = 33;
pub const B_DELETE: u32 = 34;
pub const B_RESTORE: u32 = 35;
pub const C_OPEN: u32 = 2000; // + clone index
pub const C_KEEP: u32 = 3000;
pub const C_DISCARD: u32 = 4000;
pub const SCRUB_STOP: u32 = 40;
pub const RATE: u32 = 50; // 50..52
pub const AUTO: u32 = 60; // 60..63
pub const CHECK: u32 = 70;
pub const REPAIR: u32 = 71;
pub const CHG_PASS: u32 = 80;
pub const UNLOCK: u32 = 83;
pub const BACKUP: u32 = 82;
pub const LOCK: u32 = 81;
pub const LOCK_OTHER: u32 = 84;
pub const TM_NOW: u32 = 90;
pub const TM_SETTINGS: u32 = 91;
pub const SHEET: u32 = 10_000; // sheet controls

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Snapshots,
    Clones,
    Health,
    Security,
}

const TABS: [Tab; 5] = [
    Tab::Overview,
    Tab::Snapshots,
    Tab::Clones,
    Tab::Health,
    Tab::Security,
];

pub struct App {
    pub th: Theme,
    pub disk: Disk,
    pub tab: Tab,
    /// The selected sidebar row (0 = the primary volume).
    pub vol: usize,
    pub hits: Hits,
    pub sel_snap: Option<String>,
    pub diff: Option<(String, Diff)>,
    pub sheet: Option<Sheet>,
    pub job: Option<Job>,
    /// The last job's report, shown on the tab it belongs to.
    pub report: String,
    pub check_report: String,
    pub scroll: i32,
    pub last_refresh: u64,
}

fn main(_args: &rt::Args) -> i32 {
    let Ok(mut win) = Window::new("Disk Utility", W, H) else {
        rt::println!("diskutil: no desktop to open a window on");
        return 1;
    };
    let mut app = App {
        th: Theme::current(),
        disk: Disk::default(),
        tab: Tab::Overview,
        vol: 0,
        hits: Hits::default(),
        sel_snap: None,
        diff: None,
        sheet: None,
        job: None,
        report: String::new(),
        check_report: String::new(),
        scroll: 0,
        last_refresh: 0,
    };
    // Resizable down to what its sheets need (the spec says 680x480).
    win.set_resizable(760, 560);
    app.disk.refresh();
    app.disk.share_rw = Some(data::probe_share());
    loop {
        match win.event(100) {
            Some(Event::Close) => break,
            Some(Event::Resize { w, h }) => {
                let _ = win.resize(w, h);
            }
            Some(Event::Down { x, y, button: 1 }) => {
                if let Some(id) = app.hits.at(x, y) {
                    app.click(id);
                }
            }
            Some(Event::Key(k, m)) => {
                if (m.ctrl || m.meta) && k == Key::Char('q') {
                    break;
                }
                app.key(k, m);
            }
            Some(Event::Scroll(d)) => app.scroll = (app.scroll + d * 30).max(0),
            // Settings changed the font or the theme: the next draw (below) uses them.
            Some(Event::Appearance { dark, .. }) => app.th = Theme::for_dark(dark),
            _ => {}
        }
        let now = timeio::mono_ms();
        let busy = app.job.is_some() || app.disk.scrub.as_ref().is_some_and(|s| s.running());
        let every = if busy { 1000 } else { 2000 };
        if now.saturating_sub(app.last_refresh) >= every {
            app.last_refresh = now;
            app.disk.refresh();
            app.th = Theme::current();
        }
        app.poll_job();
        app.draw(&mut win);
        win.commit();
    }
    0
}

/// One row of the sidebar.
pub struct Row {
    pub name: String,
    pub kind: String,
    pub icon: Icon,
    pub locked: bool,
    /// Where it is mounted, for usage and locking.
    pub path: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Disk,
    Clone,
    Share,
    Ram,
}

/// The size of a volume's icon slot in the sidebar (SPEC: a 32px icon).
pub const ICON: i32 = 32;

/// A volume's icon in its slot at (x, y). Today a plain tile (radius 24% of the size, as the SPEC
/// says for tiles); the art pass replaces the body of this function with the art icons, drawn at
/// `ICON` x `ICON` here, and nothing else in the window changes.
fn volume_icon(win: &mut Window, th: &Theme, x: i32, y: i32, icon: Icon, locked: bool) {
    let tile = match icon {
        Icon::Disk => th.honey,
        Icon::Clone => th.lav,
        Icon::Share => th.blue,
        Icon::Ram => th.sand,
    };
    win.rrect(x, y, ICON, ICON, ICON * 24 / 100, tile);
    if locked {
        widgets::padlock(win, x + 9, y + 8, th.ink);
    }
}

impl App {
    pub fn rows(&self) -> Vec<Row> {
        let mut v = Vec::new();
        let d = &self.disk;
        match &d.info {
            Some(i) => v.push(Row {
                name: i.label.clone(),
                kind: format!("milkfs, {}", diskkit::fmt::kb(i.total_kb())),
                icon: Icon::Disk,
                locked: false,
                path: Some(format!("/Volumes/{}", i.label)),
            }),
            None => {
                for l in &d.locked {
                    v.push(Row {
                        name: l.label.clone(),
                        kind: String::from("locked"),
                        icon: Icon::Disk,
                        locked: true,
                        path: None,
                    });
                }
            }
        }
        for c in &d.clones {
            v.push(Row {
                name: format!(
                    "{}-{}",
                    d.info.as_ref().map_or("Milk", |i| i.label.as_str()),
                    c.name
                ),
                kind: format!("clone of {}", c.origin),
                icon: Icon::Clone,
                locked: false,
                path: c.mount.clone(),
            });
        }
        for vol in d.other_volumes() {
            let size = d
                .usage_of(&vol.path)
                .map(|u| format!(", {}", diskkit::fmt::kb(u.total_kb)))
                .unwrap_or_default();
            let (kind, icon) = match vol.kind {
                VolKind::MacShare => (format!("shared with the Mac{size}"), Icon::Share),
                VolKind::Fat => (format!("FAT32{size}"), Icon::Disk),
                VolKind::Milk => (format!("milkfs{size}"), Icon::Disk),
            };
            v.push(Row {
                name: vol.name.clone(),
                kind,
                icon,
                locked: false,
                path: Some(vol.path.clone()),
            });
        }
        // Other encrypted volumes that are locked (not mounted): listed so they can be unlocked.
        if d.info.is_some() {
            for l in &d.locked {
                v.push(Row {
                    name: l.label.clone(),
                    kind: String::from("locked"),
                    icon: Icon::Disk,
                    locked: true,
                    path: None,
                });
            }
        }
        // Disks the system lists that have no row yet: a milkfs volume that is not mounted, or a disk
        // that is not milkfs at all.
        if let Some(list) = &d.disks {
            for e in list {
                match &e.content {
                    diskkit::disks::Content::Milkfs {
                        state: diskkit::disks::State::Unmounted,
                        label,
                        ..
                    } => v.push(Row {
                        name: label.clone(),
                        kind: format!("not mounted, {}", diskkit::fmt::kb(e.kb())),
                        icon: Icon::Disk,
                        locked: false,
                        path: None,
                    }),
                    diskkit::disks::Content::Other => v.push(Row {
                        name: e.name.clone(),
                        kind: format!("disk, {}", diskkit::fmt::kb(e.kb())),
                        icon: Icon::Disk,
                        locked: false,
                        path: None,
                    }),
                    _ => {}
                }
            }
        }
        v.push(Row {
            name: String::from("In-memory root"),
            kind: d.usage_of("/").map_or(String::from("RAM, not kept"), |u| {
                format!("RAM, {}", diskkit::fmt::kb(u.total_kb))
            }),
            icon: Icon::Ram,
            locked: false,
            path: Some(String::from("/")),
        });
        v
    }

    pub fn volume_label(&self) -> String {
        self.disk
            .info
            .as_ref()
            .map_or_else(|| String::from("Milk"), |i| i.label.clone())
    }

    fn click(&mut self, id: u32) {
        if self.sheet.is_some() {
            self.sheet_click(id);
            return;
        }
        match id {
            t if (TAB..TAB + 5).contains(&t) => {
                self.tab = TABS[(t - TAB) as usize];
                self.scroll = 0;
            }
            PRIMARY => self.primary_button(),
            v if (VOL..VOL + 50).contains(&v) => {
                self.vol = (v - VOL) as usize;
                self.scroll = 0;
            }
            s if (SNAP_ROW..SNAP_ROW + 100).contains(&s) => {
                if let Some(sn) = self.disk.snaps.get((s - SNAP_ROW) as usize) {
                    self.sel_snap = Some(sn.name.clone());
                    self.diff = None;
                }
            }
            _ => self.tab_click(id),
        }
    }

    fn key(&mut self, k: Key, m: gui::Mods) {
        if self.sheet.is_some() {
            self.sheet_key(k, m);
            return;
        }
        if (m.ctrl || m.meta)
            && let Key::Char(c) = k
            && ('1'..='5').contains(&c)
        {
            self.tab = TABS[c as usize - '1' as usize];
        }
    }

    // ---- drawing --------------------------------------------------------------------------

    fn draw(&mut self, win: &mut Window) {
        let th = self.th;
        self.hits.clear();
        win.fill(0, 0, win.w, win.h, th.milk);
        self.draw_sidebar(win);
        win.fill(SIDE, 0, win.w - SIDE, BAR, th.cream);
        widgets::hline(win, SIDE, BAR, win.w - SIDE, th.sand);
        let sel = TABS.iter().position(|&t| t == self.tab).unwrap_or(0);
        segmented(
            win,
            &th,
            &mut self.hits,
            SIDE + 12,
            8,
            &["Overview", "Snapshots", "Clones", "Health", "Security"],
            sel,
            TAB,
        );
        self.draw_primary(win);
        if self.vol == 0 && self.disk.info.is_some() {
            match self.tab {
                Tab::Overview => self.draw_overview(win),
                Tab::Snapshots => self.draw_snapshots(win),
                Tab::Clones => self.draw_clones(win),
                Tab::Health => self.draw_health(win),
                Tab::Security => self.draw_security(win),
            }
        } else {
            self.draw_other(win);
        }
        if let Some(j) = &self.job {
            let secs = timeio::mono_ms().saturating_sub(j.started_ms) / 1000;
            win.fill(SIDE, win.h - 24, win.w - SIDE, 24, th.cream);
            widgets::hline(win, SIDE, win.h - 24, win.w - SIDE, th.sand);
            win.text(
                SIDE + 12,
                win.h - 20,
                &format!("{} ... {} s", j.what, secs),
                th.espresso,
            );
        }
        if self.sheet.is_some() {
            self.hits.modal();
            let ink = th.ink;
            for p in win.pixels() {
                *p = blend(*p, ink, 0x47);
            }
            self.draw_sheet(win);
        }
    }

    fn draw_sidebar(&mut self, win: &mut Window) {
        let th = self.th;
        win.fill(0, 0, SIDE, win.h, th.oat);
        widgets::vline(win, SIDE - 1, 0, win.h, th.sand);
        win.text(16, 14, "VOLUMES", th.cocoa);
        let rows = self.rows();
        for (i, r) in rows.iter().enumerate() {
            let y = 40 + i as i32 * 50;
            if y + 50 > win.h {
                break;
            }
            let rect = Rect::new(8, y, SIDE - 16, 46);
            if self.vol == i {
                win.rrect(
                    rect.x,
                    rect.y,
                    rect.w,
                    rect.h,
                    10,
                    Theme::tint(th.oat, th.straw, 0x33),
                );
            }
            volume_icon(win, &th, 16, y + 7, r.icon, r.locked);
            bold_x(
                win,
                56,
                y + 7,
                &widgets::fit(&r.name, SIDE - 56 - 12),
                th.espresso,
                1,
            );
            // No secondary text on a selected row: it would sit on the straw tint.
            let kind_col = if self.vol == i { th.espresso } else { th.cocoa };
            win.text(56, y + 25, &widgets::fit(&r.kind, SIDE - 56 - 12), kind_col);
            self.hits.add(rect, VOL + i as u32);
        }
    }

    fn draw_primary(&mut self, win: &mut Window) {
        let th = self.th;
        let (label, id) = match self.tab {
            Tab::Overview | Tab::Snapshots => ("Take snapshot", PRIMARY),
            Tab::Clones => ("New clone...", PRIMARY),
            Tab::Health => ("Scrub now", PRIMARY),
            Tab::Security => return,
        };
        let enabled = self.vol == 0 && self.disk.info.is_some() && self.job.is_none();
        let scrubbing =
            self.tab == Tab::Health && self.disk.scrub.as_ref().is_some_and(|s| s.running());
        let bw = button_w(label, true);
        button(
            win,
            &th,
            &mut self.hits,
            Rect::new(win.w - 12 - bw, 10, bw, 24),
            label,
            Kind::Primary,
            id,
            enabled && !scrubbing,
            true,
        );
    }

    fn primary_button(&mut self) {
        match self.tab {
            Tab::Overview | Tab::Snapshots => self.take_snapshot(),
            Tab::Clones => self.open_new_clone(),
            Tab::Health => {
                if let Err(e) = data::ctl("scrub", "start") {
                    self.fail("Could not start the scrub", e);
                }
            }
            Tab::Security => {}
        }
    }

    fn poll_job(&mut self) {
        let Some(j) = &self.job else { return };
        let Some(code) = data::poll_job(j) else {
            return;
        };
        let (what, from) = (j.what.clone(), j.report_from);
        self.job = None;
        let report = data::text(&format!("/System/Kernel/fs/milkfs/{from}"));
        match from {
            "check" => self.check_report = report.clone(),
            _ => self.report = report.clone(),
        }
        self.disk.refresh();
        if code == 13 && (what == "Restoring" || what == "Backing up") {
            self.sheet = Some(Sheet::Message {
                title: format!("{what} did not finish"),
                body: alloc::vec![
                    String::from("The passphrase is wrong or missing for this backup."),
                    String::from("Details: error 13"),
                ],
                danger: true,
            });
        } else if code != 0 {
            self.fail(&format!("{what} did not finish"), code);
        } else if from != "check" {
            let line =
                diskkit::backup::parse_report(&report).map_or(String::from("Done."), |r| r.text);
            self.sheet = Some(Sheet::Message {
                title: format!("{what} finished"),
                body: alloc::vec![line],
                danger: false,
            });
        }
    }
}
