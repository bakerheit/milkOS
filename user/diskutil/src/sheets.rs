//! The sheets: the reusable confirm sheet for every destructive action, and the Time Machine
//! settings, new clone, back up, restore, unlock and change-passphrase sheets.

use crate::*;
use appkit::Field;
use appkit::widgets::{checkbox, chip, field, switch};
use diskkit::snaps::{self, ChangeKind};
use diskkit::{backup, fsinfo, names};

const CANCEL: u32 = SHEET;
const OK: u32 = SHEET + 1;
const CHECKBOX: u32 = SHEET + 2;
const SWITCH: u32 = SHEET + 3;
const LINK: u32 = SHEET + 4;
const FIELD: u32 = SHEET + 10; // + index
const CHIP_A: u32 = SHEET + 100; // + index
const CHIP_B: u32 = SHEET + 300; // + index

pub enum Action {
    Rollback(String),
    DeleteSnap(String),
    DiscardClone(String),
    KeepClone(String),
    Repair,
    Replace { stream: String },
}

pub struct Confirm {
    pub title: String,
    pub lines: Vec<String>,
    pub note: Option<String>,
    /// The "take a snapshot first" checkbox: (ticked, the name it will make).
    pub snap_first: Option<(bool, String)>,
    pub verb: &'static str,
    pub action: Action,
}

pub struct Tm {
    pub on: bool,
    pub interval: usize,
    pub keep: Field,
    pub daily: Field,
    pub focus: usize,
}

pub struct NewClone {
    pub snap: usize,
    pub name: Field,
    pub error: Option<&'static str>,
}

pub struct Backup {
    pub snap: usize,
    /// Index into the older snapshots, or `None` for a full backup.
    pub from: Option<usize>,
    pub path: Field,
    pub understood: bool,
    pub error: Option<&'static str>,
}

pub struct Restore {
    pub files: Vec<String>,
    pub pick: usize,
    pub replace: bool,
}

pub struct Unlock {
    pub secret: Field,
    pub recovery: bool,
    pub error: bool,
}

pub struct Passwd {
    pub fields: [Field; 3],
    pub focus: usize,
    pub error: Option<&'static str>,
}

pub enum Sheet {
    Message {
        title: String,
        body: Vec<String>,
        danger: bool,
    },
    Confirm(Confirm),
    Tm(Tm),
    NewClone(NewClone),
    Backup(Backup),
    Restore(Restore),
    Unlock(Unlock),
    Passwd(Passwd),
}

const INTERVALS: [(&str, &str); 6] = [
    ("1 min", "1m"),
    ("5 min", "5m"),
    ("15 min", "15m"),
    ("30 min", "30m"),
    ("1 hour", "1h"),
    ("2 hours", "2h"),
];

fn masked(f: &Field) -> Field {
    Field {
        text: "*".repeat(f.text.chars().count()),
        cur: f.cur,
        digits: false,
        max: f.max,
    }
}

impl App {
    fn safety_name(&self) -> String {
        let (d, sod) = timekit::date::split_secs(timeio::now_secs());
        format!("safety-{}-{:02}{:02}", d.fmt(), sod / 3600, sod / 60 % 60)
    }

    // ---- opening sheets --------------------------------------------------------------------

    pub fn open_tm_settings(&mut self) {
        let a = self.disk.auto.clone();
        let (on, interval, keep, daily) = match a {
            Some(a) => (
                a.enabled,
                INTERVALS
                    .iter()
                    .position(|i| i.1 == a.interval)
                    .unwrap_or(2),
                a.keep,
                a.daily,
            ),
            None => (true, 2, 8, 14),
        };
        self.sheet = Some(Sheet::Tm(Tm {
            on,
            interval,
            keep: Field::numeric(&format!("{keep}"), 3),
            daily: Field::numeric(&format!("{daily}"), 3),
            focus: 0,
        }));
    }

    pub fn open_new_clone(&mut self) {
        if self.disk.snaps.is_empty() {
            self.fail("There is no snapshot to clone", 2);
            return;
        }
        let snap = self
            .sel_snap
            .as_ref()
            .and_then(|n| self.disk.snaps.iter().position(|s| &s.name == n))
            .unwrap_or(self.disk.snaps.len() - 1);
        self.sheet = Some(Sheet::NewClone(NewClone {
            snap,
            name: Field::new("", 40),
            error: None,
        }));
    }

    pub fn open_backup(&mut self) {
        if self.disk.snaps.is_empty() {
            self.fail("There is no snapshot to back up", 2);
            return;
        }
        self.disk.share_rw = Some(data::probe_share());
        let snap = self
            .sel_snap
            .as_ref()
            .and_then(|n| self.disk.snaps.iter().position(|s| &s.name == n))
            .unwrap_or(self.disk.snaps.len() - 1);
        let path = backup::path(&self.disk.snaps[snap].name, None);
        self.sheet = Some(Sheet::Backup(Backup {
            snap,
            from: None,
            path: Field::new(&path, 120),
            understood: false,
            error: None,
        }));
    }

    pub fn open_restore(&mut self) {
        let files = data::backup_files();
        self.sheet = Some(Sheet::Restore(Restore {
            files,
            pick: 0,
            replace: false,
        }));
    }

    pub fn open_passwd(&mut self) {
        self.sheet = Some(Sheet::Passwd(Passwd {
            fields: [Field::new("", 80), Field::new("", 80), Field::new("", 80)],
            focus: 0,
            error: None,
        }));
    }

    pub fn open_unlock(&mut self) {
        self.sheet = Some(Sheet::Unlock(Unlock {
            secret: Field::new("", 80),
            recovery: false,
            error: false,
        }));
    }

    pub fn ask_rollback(&mut self) {
        let Some(name) = self.sel_snap.clone() else {
            return;
        };
        let lost = snaps::lost_by_rollback(&self.disk.snaps, &name).len();
        let when = self
            .disk
            .snaps
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.when())
            .unwrap_or_default();
        let mut lines = Vec::new();
        match data::ctl("diff", &snaps::diff_request(&name, None)) {
            Ok(()) => {
                let d = snaps::parse_diff(&data::text("/sys/fs/milkfs/diff"));
                let n = |k: ChangeKind| d.changes.iter().filter(|c| c.kind == k).count();
                lines.push(format!(
                    "{} change{} since then ({} added, {} changed, {} deleted, {} moved) will be undone.",
                    d.changes.len(),
                    if d.changes.len() == 1 { "" } else { "s" },
                    n(ChangeKind::Added),
                    n(ChangeKind::Modified),
                    n(ChangeKind::Removed),
                    n(ChangeKind::Renamed)
                ));
            }
            Err(_) => lines.push(String::from(
                "Everything changed since then will be undone.",
            )),
        }
        lines.push(format!("The volume goes back to how it was on {when}."));
        if lost > 0 {
            lines.push(format!(
                "{lost} newer snapshot{} will be deleted too (use Send first to keep a copy).",
                if lost == 1 { "" } else { "s" }
            ));
        }
        // No "snapshot first" box here: a rollback deletes every newer snapshot, the safety one too.
        self.sheet = Some(Sheet::Confirm(Confirm {
            title: format!("Roll back to {name}?"),
            lines,
            note: Some(String::from(
                "Apps with files open on the volume may be affected.",
            )),
            snap_first: None,
            verb: "Roll back",
            action: Action::Rollback(name),
        }));
    }

    pub fn ask_delete(&mut self) {
        let Some(name) = self.sel_snap.clone() else {
            return;
        };
        self.sheet = Some(Sheet::Confirm(Confirm {
            title: format!("Delete the snapshot {name}?"),
            lines: alloc::vec![
                String::from("The snapshot is deleted for good; space only it used is freed."),
                String::from("The live volume is not changed.")
            ],
            note: None,
            snap_first: None,
            verb: "Delete snapshot",
            action: Action::DeleteSnap(name),
        }));
    }

    pub fn ask_discard(&mut self, i: usize) {
        let Some(c) = self.disk.clones.get(i) else {
            return;
        };
        let name = c.name.clone();
        self.sheet = Some(Sheet::Confirm(Confirm {
            title: format!("Discard the clone {name}?"),
            lines: alloc::vec![
                String::from("Everything written in the clone is freed and can not come back."),
                String::from("The snapshot it was made from is kept.")
            ],
            note: None,
            snap_first: None,
            verb: "Discard clone",
            action: Action::DiscardClone(name),
        }));
    }

    pub fn ask_keep(&mut self, i: usize) {
        let Some(c) = self.disk.clones.get(i) else {
            return;
        };
        let name = c.name.clone();
        let safety = self.safety_name();
        self.sheet = Some(Sheet::Confirm(Confirm {
            title: format!("Keep the clone {name}?"),
            lines: alloc::vec![
                format!(
                    "The clone becomes the live volume; what you have now is kept as the snapshot before-{name}."
                ),
                format!("Roll back to before-{name} undoes this."),
            ],
            note: Some(String::from(
                "Apps with files open on the volume may be affected.",
            )),
            snap_first: Some((true, safety)),
            verb: "Keep clone",
            action: Action::KeepClone(name),
        }));
    }

    pub fn ask_repair(&mut self) {
        let safety = self.safety_name();
        self.sheet = Some(Sheet::Confirm(Confirm {
            title: String::from("Repair the volume?"),
            lines: alloc::vec![
                String::from(
                    "Repair rewrites the volume's structure where that is safe. Run Check first to see what it would do."
                ),
                String::from("Files it can not repair are left as they are.")
            ],
            note: Some(String::from(
                "Apps with files open on the volume may be affected.",
            )),
            snap_first: Some((true, safety)),
            verb: "Repair",
            action: Action::Repair,
        }));
    }

    // ---- doing things ----------------------------------------------------------------------

    fn confirmed(&mut self, c: Confirm) {
        if let Some((true, name)) = &c.snap_first
            && let Err(e) = data::ctl("snapshot", name)
        {
            self.fail("Could not take the safety snapshot", e);
            return;
        }
        let r = match &c.action {
            Action::Rollback(n) => data::ctl("rollback", n),
            Action::DeleteSnap(n) => data::ctl("delete", n),
            Action::DiscardClone(n) => data::ctl("discard", n),
            Action::KeepClone(n) => data::ctl("promote", n),
            Action::Repair => {
                self.check_report = String::new();
                self.job =
                    data::spawn_write("Repairing the volume", "check", "repair", timeio::mono_ms());
                Ok(())
            }
            Action::Replace { stream } => {
                self.job = data::spawn_write(
                    "Restoring",
                    "receive",
                    &backup::receive_request(stream, None, true),
                    timeio::mono_ms(),
                );
                Ok(())
            }
        };
        match r {
            Ok(()) => {
                self.diff = None;
                if matches!(c.action, Action::DeleteSnap(_) | Action::Rollback(_)) {
                    self.sel_snap = None;
                }
                self.disk.refresh();
            }
            Err(e) => self.fail(&format!("{} did not work", c.verb), e),
        }
    }

    pub fn sheet_click(&mut self, id: u32) {
        let Some(sheet) = self.sheet.take() else {
            return;
        };
        match sheet {
            Sheet::Message { .. } => {
                // Any button closes it.
            }
            Sheet::Confirm(mut c) => match id {
                CANCEL => {}
                CHECKBOX => {
                    if let Some((on, _)) = &mut c.snap_first {
                        *on = !*on;
                    }
                    self.sheet = Some(Sheet::Confirm(c));
                }
                OK => self.confirmed(c),
                _ => self.sheet = Some(Sheet::Confirm(c)),
            },
            Sheet::Tm(mut t) => match id {
                CANCEL => {}
                SWITCH => {
                    t.on = !t.on;
                    self.sheet = Some(Sheet::Tm(t));
                }
                OK => {
                    let text = format!(
                        "{} interval={} keep={} daily={}",
                        if t.on { "on" } else { "off" },
                        INTERVALS[t.interval].1,
                        t.keep.text.parse::<u32>().unwrap_or(8).max(1),
                        t.daily.text.parse::<u32>().unwrap_or(14)
                    );
                    if let Err(e) = data::ctl("auto", &text) {
                        self.fail("Could not change Time Machine", e);
                    }
                    self.disk.refresh();
                }
                c if (CHIP_A..CHIP_A + 6).contains(&c) => {
                    t.interval = (c - CHIP_A) as usize;
                    self.sheet = Some(Sheet::Tm(t));
                }
                f if (FIELD..FIELD + 2).contains(&f) => {
                    t.focus = (f - FIELD) as usize;
                    self.sheet = Some(Sheet::Tm(t));
                }
                _ => self.sheet = Some(Sheet::Tm(t)),
            },
            Sheet::NewClone(mut n) => match id {
                CANCEL => {}
                OK => match names::check_name(&n.name.text) {
                    Err(e) => {
                        n.error = Some(e);
                        self.sheet = Some(Sheet::NewClone(n));
                    }
                    Ok(()) if self.disk.clones.iter().any(|c| c.name == n.name.text) => {
                        n.error = Some("A clone has that name");
                        self.sheet = Some(Sheet::NewClone(n));
                    }
                    Ok(()) => {
                        let req = snaps::clone_request(&self.disk.snaps[n.snap].name, &n.name.text);
                        match data::ctl("clone", &req) {
                            Ok(()) => {
                                self.tab = Tab::Clones;
                                self.disk.refresh();
                            }
                            Err(e) => self.fail("Could not make the clone", e),
                        }
                    }
                },
                c if (CHIP_A..CHIP_A + 100).contains(&c) => {
                    n.snap = (c - CHIP_A) as usize;
                    self.sheet = Some(Sheet::NewClone(n));
                }
                _ => self.sheet = Some(Sheet::NewClone(n)),
            },
            Sheet::Backup(mut b) => match id {
                CANCEL => {}
                CHECKBOX => {
                    b.understood = !b.understood;
                    self.sheet = Some(Sheet::Backup(b));
                }
                OK => {
                    let ready = b.understood && self.disk.share_rw == Some(true);
                    if !ready {
                        self.sheet = Some(Sheet::Backup(b));
                    } else {
                        let from = b.from.map(|i| self.disk.snaps[i].name.clone());
                        let snap = self.disk.snaps[b.snap].name.clone();
                        let req = backup::send_request(&snap, from.as_deref(), &b.path.text);
                        let _ = data::ctl("send", "");
                        self.job = data::spawn_write("Backing up", "send", &req, timeio::mono_ms());
                    }
                }
                c if (CHIP_A..CHIP_A + 100).contains(&c) => {
                    b.snap = (c - CHIP_A) as usize;
                    b.from = None;
                    let p = backup::path(&self.disk.snaps[b.snap].name, None);
                    b.path.set(&p);
                    self.sheet = Some(Sheet::Backup(b));
                }
                c if (CHIP_B..CHIP_B + 100).contains(&c) => {
                    let k = (c - CHIP_B) as usize;
                    b.from = if k == 0 { None } else { Some(k - 1) };
                    let from = b.from.map(|i| self.disk.snaps[i].name.clone());
                    let p = backup::path(&self.disk.snaps[b.snap].name, from.as_deref());
                    b.path.set(&p);
                    self.sheet = Some(Sheet::Backup(b));
                }
                _ => self.sheet = Some(Sheet::Backup(b)),
            },
            Sheet::Restore(mut r) => match id {
                CANCEL => {}
                SWITCH => {
                    r.replace = !r.replace;
                    self.sheet = Some(Sheet::Restore(r));
                }
                OK => {
                    if let Some(f) = r.files.get(r.pick) {
                        let stream = format!("{}/{f}", backup::DIR);
                        if r.replace {
                            self.sheet = Some(Sheet::Confirm(Confirm {
                                title: String::from("Replace what is on the volume?"),
                                lines: alloc::vec![
                                    format!(
                                        "The volume is rolled back to the point {f} is based on, then the backup is applied."
                                    ),
                                    String::from("Newer snapshots are deleted.")
                                ],
                                note: Some(String::from(
                                    "Apps with files open on the volume may be affected.",
                                )),
                                snap_first: None,
                                verb: "Replace",
                                action: Action::Replace { stream },
                            }));
                        } else {
                            let _ = data::ctl("receive", "");
                            self.job = data::spawn_write(
                                "Restoring",
                                "receive",
                                &backup::receive_request(&stream, None, false),
                                timeio::mono_ms(),
                            );
                        }
                    }
                }
                c if (CHIP_A..CHIP_A + 100).contains(&c) => {
                    r.pick = (c - CHIP_A) as usize;
                    self.sheet = Some(Sheet::Restore(r));
                }
                _ => self.sheet = Some(Sheet::Restore(r)),
            },
            Sheet::Unlock(mut u) => match id {
                CANCEL => {}
                LINK => {
                    u.recovery = !u.recovery;
                    u.secret.set("");
                    self.sheet = Some(Sheet::Unlock(u));
                }
                OK => {
                    let dev = self.disk.locked.first().map(|l| l.device.clone());
                    match data::ctl(
                        "unlock",
                        &fsinfo::unlock_request(dev.as_deref(), &u.secret.text),
                    ) {
                        Ok(()) => self.disk.refresh(),
                        Err(13) => {
                            u.error = true;
                            u.secret.set("");
                            self.sheet = Some(Sheet::Unlock(u));
                        }
                        Err(e) => self.fail("Could not unlock", e),
                    }
                }
                _ => self.sheet = Some(Sheet::Unlock(u)),
            },
            Sheet::Passwd(mut p) => match id {
                CANCEL => {}
                OK => match fsinfo::check_passphrase(&p.fields[1].text, &p.fields[2].text) {
                    Err(e) => {
                        p.error = Some(e);
                        self.sheet = Some(Sheet::Passwd(p));
                    }
                    Ok(()) => match data::ctl(
                        "passwd",
                        &fsinfo::passwd_request(&p.fields[0].text, &p.fields[1].text),
                    ) {
                        Ok(()) => {
                            self.sheet = Some(Sheet::Message {
                                title: String::from("Passphrase changed"),
                                body: alloc::vec![String::from(
                                    "The key was rewrapped; no data was rewritten."
                                )],
                                danger: false,
                            });
                        }
                        Err(13) => {
                            p.error = Some("That was not the current passphrase");
                            self.sheet = Some(Sheet::Passwd(p));
                        }
                        Err(e) => self.fail("Could not change the passphrase", e),
                    },
                },
                f if (FIELD..FIELD + 3).contains(&f) => {
                    p.focus = (f - FIELD) as usize;
                    self.sheet = Some(Sheet::Passwd(p));
                }
                _ => self.sheet = Some(Sheet::Passwd(p)),
            },
        }
    }

    pub fn sheet_key(&mut self, k: Key, m: gui::Mods) {
        match &mut self.sheet {
            Some(Sheet::Message { .. }) => {
                if matches!(k, Key::Enter | Key::Escape) {
                    self.sheet = None;
                }
            }
            // Return and Esc both cancel a confirm: the destructive button is never the default.
            Some(Sheet::Confirm(_)) => {
                if matches!(k, Key::Enter | Key::Escape) {
                    self.sheet = None;
                }
            }
            Some(Sheet::Tm(t)) => match k {
                Key::Escape => self.sheet = None,
                Key::Enter => self.sheet_click(OK),
                Key::Tab => t.focus = 1 - t.focus,
                _ => {
                    let f = if t.focus == 0 {
                        &mut t.keep
                    } else {
                        &mut t.daily
                    };
                    f.key(k, m);
                }
            },
            Some(Sheet::NewClone(n)) => match k {
                Key::Escape => self.sheet = None,
                Key::Enter => self.sheet_click(OK),
                _ => {
                    n.error = None;
                    n.name.key(k, m);
                }
            },
            Some(Sheet::Backup(b)) => match k {
                Key::Escape => self.sheet = None,
                _ => {
                    b.path.key(k, m);
                }
            },
            Some(Sheet::Restore(_)) => {
                if k == Key::Escape {
                    self.sheet = None;
                }
            }
            Some(Sheet::Unlock(u)) => match k {
                Key::Escape => self.sheet = None,
                Key::Enter => self.sheet_click(OK),
                _ => {
                    u.error = false;
                    u.secret.key(k, m);
                }
            },
            Some(Sheet::Passwd(p)) => match k {
                Key::Escape => self.sheet = None,
                Key::Enter => self.sheet_click(OK),
                Key::Tab => p.focus = (p.focus + 1) % 3,
                _ => {
                    p.error = None;
                    let i = p.focus;
                    p.fields[i].key(k, m);
                }
            },
            None => {}
        }
    }

    // ---- drawing ---------------------------------------------------------------------------

    fn frame(&self, win: &mut Window, w: i32, h: i32, title: &str, danger: bool) -> Rect {
        let th = self.th;
        let r = Rect::new((win.w - w) / 2, (win.h - h) / 2, w, h);
        win.rrect(
            r.x - 1,
            r.y + 6,
            r.w + 2,
            r.h + 2,
            16,
            blend(th.milk, th.ink, 0x33),
        );
        widgets::bordered(win, r, 14, th.milk, th.sand);
        if danger {
            // The Danger header: 44 high, the title as a question in #FFFDF8.
            win.rrect(r.x, r.y, r.w, 44, 14, th.danger);
            win.fill(r.x, r.y + 22, r.w, 22, th.danger);
            bold_x(
                win,
                r.x + 20,
                r.y + 12,
                &widgets::fit(title, r.w - 40),
                th.on_danger,
                1,
            );
        } else {
            bold_x(win, r.x + 20, r.y + 14, title, th.espresso, 2);
        }
        r
    }

    fn buttons(&mut self, win: &mut Window, r: Rect, ok: &str, ok_kind: Kind, ok_on: bool) {
        let th = self.th;
        let by = r.bottom() - 44;
        let ow = button_w(ok, false);
        button(
            win,
            &th,
            &mut self.hits,
            Rect::new(r.right() - 20 - ow, by, ow, 28),
            ok,
            ok_kind,
            OK,
            ok_on,
            false,
        );
        let cw = button_w("Cancel", false);
        button(
            win,
            &th,
            &mut self.hits,
            Rect::new(r.right() - 20 - ow - 8 - cw, by, cw, 28),
            "Cancel",
            Kind::Secondary,
            CANCEL,
            true,
            false,
        );
    }

    pub fn draw_sheet(&mut self, win: &mut Window) {
        let th = self.th;
        let Some(sheet) = self.sheet.take() else {
            return;
        };
        let caret = timeio::mono_ms() / 500 % 2 == 0;
        match &sheet {
            Sheet::Message {
                title,
                body,
                danger,
            } => {
                let r = self.frame(win, 560, 130 + 22 * body.len() as i32, title, false);
                for (i, l) in body.iter().enumerate() {
                    win.text(
                        r.x + 20,
                        r.y + 60 + i as i32 * 22,
                        &widgets::fit(l, r.w - 40),
                        if *danger && i == 0 {
                            th.danger
                        } else {
                            th.espresso
                        },
                    );
                }
                let w = button_w("OK", false);
                button(
                    win,
                    &th,
                    &mut self.hits,
                    Rect::new(r.right() - 20 - w, r.bottom() - 44, w, 28),
                    "OK",
                    Kind::Primary,
                    CANCEL,
                    true,
                    false,
                );
            }
            Sheet::Confirm(c) => {
                let extra = if c.note.is_some() { 40 } else { 0 }
                    + if c.snap_first.is_some() { 56 } else { 0 };
                let h = 44 + 28 + c.lines.len() as i32 * 22 + extra + 76;
                let r = self.frame(win, 680, h, &c.title, true);
                let mut y = r.y + 44 + 20;
                for l in &c.lines {
                    win.text(r.x + 20, y, &widgets::fit(l, r.w - 40), th.espresso);
                    y += 22;
                }
                y += 8;
                if let Some((on, name)) = &c.snap_first {
                    checkbox(
                        win,
                        &th,
                        &mut self.hits,
                        r.x + 20,
                        y,
                        *on,
                        "Take a snapshot of the volume as it is now first",
                        CHECKBOX,
                    );
                    win.text(
                        r.x + 46,
                        y + 20,
                        &format!("It will be called {name}"),
                        th.cocoa,
                    );
                    y += 54;
                }
                if let Some(n) = &c.note {
                    let nr = Rect::new(r.x + 20, y, r.w - 40, 28);
                    widgets::bordered(win, nr, 8, Theme::tint(th.milk, th.honey, 0x22), th.honey);
                    win.text(
                        nr.x + 10,
                        nr.y + 6,
                        &widgets::fit(n, nr.w - 20),
                        th.espresso,
                    );
                }
                // Cancel is the default (Return and Esc); the verb button is Danger.
                self.buttons(win, r, c.verb, Kind::Danger, true);
            }
            Sheet::Tm(t) => {
                let r = self.frame(win, 520, 330, "Time Machine settings", false);
                let x = r.x + 20;
                win.text(x, r.y + 64, "Snapshot automatically", th.espresso);
                switch(
                    win,
                    &th,
                    &mut self.hits,
                    r.right() - 20 - 44,
                    r.y + 60,
                    t.on,
                    SWITCH,
                );
                win.text(x, r.y + 100, "EVERY", th.cocoa);
                let mut cx = x;
                for (i, (l, _)) in INTERVALS.iter().enumerate() {
                    cx += chip(
                        win,
                        &th,
                        &mut self.hits,
                        cx,
                        r.y + 120,
                        l,
                        t.interval == i,
                        CHIP_A + i as u32,
                    ) + 6;
                }
                win.text(x, r.y + 170, "KEEP THE LAST", th.cocoa);
                field(
                    win,
                    &th,
                    &mut self.hits,
                    Rect::new(x, r.y + 190, 80, 28),
                    &t.keep,
                    "8",
                    t.focus == 0,
                    caret,
                    FIELD,
                );
                win.text(x + 100, r.y + 196, "snapshots", th.cocoa);
                win.text(x, r.y + 232, "AND ONE A DAY FOR", th.cocoa);
                field(
                    win,
                    &th,
                    &mut self.hits,
                    Rect::new(x, r.y + 252, 80, 28),
                    &t.daily,
                    "14",
                    t.focus == 1,
                    caret,
                    FIELD + 1,
                );
                win.text(x + 100, r.y + 258, "days", th.cocoa);
                self.buttons(win, r, "Save", Kind::Primary, true);
            }
            Sheet::NewClone(n) => {
                let r = self.frame(win, 560, 330, "New clone", false);
                let x = r.x + 20;
                win.text(x, r.y + 60, "FROM THE SNAPSHOT", th.cocoa);
                let mut cx = x;
                let mut cy = r.y + 82;
                for (i, s) in self.disk.snaps.iter().enumerate().rev().take(8) {
                    let w = gui::str_width(&widgets::fit(&s.name, 160), 1) + 24;
                    if cx + w > r.right() - 20 {
                        cx = x;
                        cy += 30;
                    }
                    cx += chip(
                        win,
                        &th,
                        &mut self.hits,
                        cx,
                        cy,
                        &widgets::fit(&s.name, 160),
                        n.snap == i,
                        CHIP_A + i as u32,
                    ) + 6;
                }
                win.text(x, r.y + 190, "NAME", th.cocoa);
                field(
                    win,
                    &th,
                    &mut self.hits,
                    Rect::new(x, r.y + 210, r.w - 40, 28),
                    &n.name,
                    "fortune",
                    true,
                    caret,
                    FIELD,
                );
                if let Some(e) = n.error {
                    win.text(x, r.y + 246, e, th.danger);
                }
                self.buttons(win, r, "Make clone", Kind::Primary, true);
            }
            Sheet::Backup(b) => {
                let r = self.frame(win, 600, 470, "Back up", false);
                let x = r.x + 20;
                win.text(x, r.y + 58, "SNAPSHOT", th.cocoa);
                let mut cx = x;
                let mut cy = r.y + 78;
                for (i, s) in self.disk.snaps.iter().enumerate().rev().take(8) {
                    let l = widgets::fit(&s.name, 150);
                    let w = gui::str_width(&l, 1) + 24;
                    if cx + w > r.right() - 20 {
                        cx = x;
                        cy += 30;
                    }
                    cx += chip(
                        win,
                        &th,
                        &mut self.hits,
                        cx,
                        cy,
                        &l,
                        b.snap == i,
                        CHIP_A + i as u32,
                    ) + 6;
                }
                win.text(x, r.y + 150, "ONLY WHAT CHANGED SINCE (OPTIONAL)", th.cocoa);
                let mut cx = x;
                let mut cy = r.y + 170;
                cx += chip(
                    win,
                    &th,
                    &mut self.hits,
                    cx,
                    cy,
                    "Everything",
                    b.from.is_none(),
                    CHIP_B,
                ) + 6;
                for (i, s) in self
                    .disk
                    .snaps
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i < b.snap)
                {
                    let l = widgets::fit(&s.name, 150);
                    let w = gui::str_width(&l, 1) + 24;
                    if cx + w > r.right() - 20 {
                        cx = x;
                        cy += 30;
                    }
                    cx += chip(
                        win,
                        &th,
                        &mut self.hits,
                        cx,
                        cy,
                        &l,
                        b.from == Some(i),
                        CHIP_B + 1 + i as u32,
                    ) + 6;
                }
                win.text(x, r.y + 236, "SAVE TO", th.cocoa);
                field(
                    win,
                    &th,
                    &mut self.hits,
                    Rect::new(x, r.y + 256, r.w - 40, 28),
                    &b.path,
                    "",
                    true,
                    caret,
                    FIELD,
                );
                let wr = Rect::new(x, r.y + 296, r.w - 40, 56);
                widgets::bordered(win, wr, 10, Theme::tint(th.milk, th.honey, 0x22), th.honey);
                win.text(
                    wr.x + 12,
                    wr.y + 8,
                    "This backup is not encrypted.",
                    th.espresso,
                );
                win.text(
                    wr.x + 12,
                    wr.y + 28,
                    "Anyone who gets the file can read everything in it.",
                    th.espresso,
                );
                checkbox(
                    win,
                    &th,
                    &mut self.hits,
                    x,
                    r.y + 366,
                    b.understood,
                    "I understand",
                    CHECKBOX,
                );
                let rw = self.disk.share_rw == Some(true);
                if !rw {
                    win.text(x, r.y + 394, "The Mac folder is read-only. Start milkOS with MILK_SHARE_RW=1 to back up to the Mac.", th.danger);
                } else if let Some(e) = b.error {
                    win.text(x, r.y + 394, e, th.danger);
                }
                self.buttons(win, r, "Send", Kind::Primary, b.understood && rw);
            }
            Sheet::Restore(rs) => {
                let r = self.frame(win, 560, 330, "Restore from file", false);
                let x = r.x + 20;
                win.text(
                    x,
                    r.y + 60,
                    "BACKUP FILES IN /Volumes/Mac/backups",
                    th.cocoa,
                );
                if rs.files.is_empty() {
                    win.text(x, r.y + 90, "No backup files there.", th.cocoa);
                }
                let mut cx = x;
                let mut cy = r.y + 82;
                for (i, f) in rs.files.iter().enumerate().take(8) {
                    let l = widgets::fit(f, 240);
                    let w = gui::str_width(&l, 1) + 24;
                    if cx + w > r.right() - 20 {
                        cx = x;
                        cy += 30;
                    }
                    cx += chip(
                        win,
                        &th,
                        &mut self.hits,
                        cx,
                        cy,
                        &l,
                        rs.pick == i,
                        CHIP_A + i as u32,
                    ) + 6;
                }
                win.text(x, r.y + 200, "Replace existing", th.espresso);
                switch(
                    win,
                    &th,
                    &mut self.hits,
                    x + 160,
                    r.y + 196,
                    rs.replace,
                    SWITCH,
                );
                win.text(
                    x,
                    r.y + 230,
                    if rs.replace {
                        "Rolls the volume back to the backup's base first (asks to confirm)."
                    } else {
                        "Adds the backup's snapshot; refuses if the volume changed since its base."
                    },
                    th.cocoa,
                );
                self.buttons(win, r, "Receive", Kind::Primary, !rs.files.is_empty());
            }
            Sheet::Unlock(u) => {
                let label = self
                    .disk
                    .locked
                    .first()
                    .map_or_else(|| self.volume_label(), |l| l.label.clone());
                let r = self.frame(win, 500, 316, "Unlock", false);
                let x = r.x + 20;
                widgets::padlock(win, x, r.y + 58, th.espresso);
                bold_x(
                    win,
                    x + 26,
                    r.y + 58,
                    &format!("{label} is encrypted and locked"),
                    th.espresso,
                    1,
                );
                win.text(
                    x,
                    r.y + 96,
                    if u.recovery {
                        "RECOVERY KEY"
                    } else {
                        "PASSPHRASE"
                    },
                    th.cocoa,
                );
                field(
                    win,
                    &th,
                    &mut self.hits,
                    Rect::new(x, r.y + 116, r.w - 40, 28),
                    &masked(&u.secret),
                    "",
                    true,
                    caret,
                    FIELD,
                );
                if u.error {
                    win.text(x, r.y + 152, "That was not the passphrase", th.danger);
                }
                win.text(
                    x,
                    r.y + 180,
                    if u.recovery {
                        "The recovery key is 8 groups of 4."
                    } else {
                        "Three wrong tries and you wait a moment."
                    },
                    th.cocoa,
                );
                let l = if u.recovery {
                    "Use the passphrase instead"
                } else {
                    "Lost it? Use the recovery key instead"
                };
                win.text(x, r.y + 208, l, th.blue);
                self.hits
                    .add(Rect::new(x, r.y + 204, gui::str_width(l, 1), 22), LINK);
                self.buttons(win, r, "Unlock", Kind::Primary, !u.secret.text.is_empty());
            }
            Sheet::Passwd(p) => {
                let r = self.frame(win, 500, 380, "Change passphrase", false);
                let x = r.x + 20;
                for (i, l) in [
                    "CURRENT PASSPHRASE (OR RECOVERY KEY)",
                    "NEW PASSPHRASE",
                    "NEW PASSPHRASE AGAIN",
                ]
                .iter()
                .enumerate()
                {
                    let y = r.y + 58 + i as i32 * 62;
                    win.text(x, y, l, th.cocoa);
                    field(
                        win,
                        &th,
                        &mut self.hits,
                        Rect::new(x, y + 20, r.w - 40, 28),
                        &masked(&p.fields[i]),
                        "",
                        p.focus == i,
                        caret,
                        FIELD + i as u32,
                    );
                }
                win.text(
                    x,
                    r.y + 250,
                    "At least 8 characters. The key is rewrapped; no data is rewritten.",
                    th.cocoa,
                );
                if let Some(e) = p.error {
                    win.text(x, r.y + 276, e, th.danger);
                }
                let _ = Key::Tab;
                self.buttons(
                    win,
                    r,
                    "Change",
                    Kind::Primary,
                    !p.fields[0].text.is_empty(),
                );
            }
        }
        self.sheet = Some(sheet);
    }
}
