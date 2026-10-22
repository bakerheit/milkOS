// Specs and mockups for Disk Utility and Reminders. Run: node docs/ui-concepts/disk-reminders.mjs
// Writes disk-reminders.html (mockups) and SPEC-DISK-REMINDERS.md (the spec, numbers computed from the tokens).
import { B, T, TC, TR, TRi, icon, drop, C, X2, X3, BD } from "./scenes.mjs";
import { TOK, DARK, WP, glass, shadow as shadowLayers, menubar, dock, lightsX, dim } from "./v1.mjs";
import { appWin, base, btn, pill, chip, seg, searchBox, gauge, circle, INK, L, ratio, mixHex, CH } from "./apps.mjs";
import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const shadow = (x, y, w, h, r, layers = TOK.shadow.window) => shadowLayers(x, y, w, h, r, layers);
const DANGER = "#B5473B";
const S = "#6B5846"; // secondary text in the light theme
const label = (x, y, t) => T(x, y, t, `color:${S}`);
const sheet = (x, y, w, hh, title, inner, hdr = null) => shadow(x, y, w, hh, 14) + B(x, y, w, hh, `background:${L.milk};border:1px solid ${L.sand};border-radius:14px;overflow:hidden`,
  (hdr || B(0, 0, w, 44, `background:${L.cream};border-bottom:1px solid ${L.sand}`)) + T(16, 14, `<b>${title}</b>`, hdr ? `color:#FFFDF8` : "") + inner);
const check = (x, y, on, txt) => B(x, y, 18, 18, `background:${on ? C.straw : L.milk};border:2px solid ${on ? C.straw : L.latte};border-radius:5px`, on ? T(2, 0, "&#10003;", `color:${INK};font-weight:700;font-size:13px;line-height:14px`) : "") + T(x + 28, y + 1, txt);
const field = (x, y, w, txt, ph = false) => B(x, y, w, 28, `background:${L.cream};border:1px solid ${L.sand};border-radius:8px`, T(10, 6, txt, ph ? `color:${S}` : ""));

/* ============================== DISK UTILITY ============================== */
const VOLS = [["Milk", "volume"], ["Milk-fortune", "clone"], ["Spare", "volume"], ["Mac", "shared folder, read-only"], ["Memory", "in-memory root"]];
const DISKS = [["Backup", "locked"], ["vdb", "not formatted"]];
const kindOf = (t) => (t.length > 14 ? t.slice(0, 13) + "~" : t);
const off = (x, y, w, txt) => B(x, y, w, 28, `border:1px solid ${L.sand};border-radius:8px`, TC(0, 6, w, `<b>${txt}</b>`, `color:${L.latte}`));
const offPill = (x, y, w, txt) => B(x, y, w, 24, `border:1px solid ${L.sand};border-radius:12px`, TC(0, 4, w, `<b>${txt}</b>`, `color:${L.latte}`));
const veil = () => B(0, 0, 1024, 768, `background:rgba(58,46,39,.28)`);
const dots = (n) => "&#8226;".repeat(n);
function duFrame(selTab, bodyFn, opts = {}) {
  let h = base("Disk Utility") + dock([0]);
  const x = 50, y = 44, w = 900, hh = 640, sw = 188, sel = opts.sel ?? 0;
  let b = B(0, 32, sw, hh - 34, `background:${L.oat};border-right:1px solid ${L.sand}`) + label(16, 46, "VOLUMES");
  const row = (i, vy, name, kind, ic) => (i === sel ? B(8, vy - 4, sw - 16, 48, `background:${C.straw}33;border-radius:10px`) : "") + ic + T(54, vy, `<b>${name}</b>`) + T(54, vy + 20, kindOf(opts.kinds?.[i] ?? kind), `color:${S}`);
  VOLS.forEach((v, i) => { const vy = 70 + i * 52; b += row(i, vy, v[0], v[1], icon(14, vy + 2, 32, v[1] === "volume" ? "gear" : "folder", v[1] === "clone" ? C.lav : v[1] === "volume" ? C.latte : C.cream, C.esp, v[1] === "shared folder, read-only" ? `border:1px solid ${L.sand}` : "")); });
  b += label(16, 336, "DISKS");
  DISKS.forEach((d, j) => { const vy = 360 + j * 52; b += row(VOLS.length + j, vy, d[0], d[1], icon(14, vy + 2, 32, "gear", C.sand, C.esp)); });
  const tabs = opts.tabs !== false;
  b += B(sw, 32, w - sw, 44, `background:${L.cream};border-bottom:1px solid ${L.sand}`) + (tabs ? seg(sw + 12, 40, ["Overview", "Snapshots", "Clones", "Health", "Security"], selTab, 84) : "") + (opts.toolbar ?? (tabs ? btn(w - 160, 38, 148, "Take snapshot", "primary") : ""));
  b += bodyFn(sw, w, hh);
  return { h, win: (extra = "") => appWin(x, y, w, hh, "Disk Utility", b + extra), x, y, w, hh, sw };
}
function duOverview() {
  const f = duFrame(0, (sw, w, hh) => {
    let b = T(sw + 24, 92, "Milk", X2 + BD) + label(sw + 24, 130, "milkfs, 8.0 GB, encrypted and unlocked");
    b += B(sw + 24, 160, w - sw - 48, 24, `background:${L.oat};border:1px solid ${L.sand};border-radius:12px;overflow:hidden`, B(0, 0, (w - sw - 48) * 0.15, 24, `background:${C.honey}`));
    b += T(sw + 24, 192, "<b>1.2 GB used</b>   6.8 GB free");
    [["Snapshots", "14"], ["Clones", "1"], ["Format", "v3 (encrypted)"], ["Last scrub", "3 days ago"]].forEach((k, i) => { b += B(sw + 24 + i * 168, 226, 156, 64, `background:${L.cream};border:1px solid ${L.sand};border-radius:12px`, label(12, 10, k[0]) + T(12, 34, `<b>${k[1]}</b>`)); });
    b += B(sw + 24, 312, w - sw - 48, 110, `background:${L.cream};border:1px solid ${L.sand};border-radius:14px`, T(16, 12, "<b>Time Machine</b>") + T(16, 36, "On   every 15 minutes   keeps the last 8 and one a day for 14 days", `color:${S}`) + T(16, 58, "Next snapshot in 12 minutes", `color:${S}`) + pill(16, 78, 130, "Snapshot now", "secondary") + pill(156, 78, 100, "Settings", "secondary"));
    b += B(sw + 24, 438, w - sw - 48, 96, `background:${C.matcha}22;border:1px solid ${C.matcha};border-radius:14px`, T(16, 12, "<b>&#10003; No problems found</b>") + T(16, 36, "The last full scrub read every block and every checksum matched.", `color:${S}`) + pill(16, 62, 100, "Scrub now", "secondary"));
    return b;
  });
  return f.h + f.win();
}
const SNAPS = [["auto-2026-10-21T14:30:12", "Today 14:30", "Auto", 0], ["auto-2026-10-21T14:15:08", "Today 14:15", "Auto", 0], ["before-fortune", "Today 13:02", "Before install", 1], ["monday", "Mon 09:00", "Manual", 0], ["auto-2026-10-19T23:45:00", "Mon 23:45", "Auto", 0]];
function duSnapshots() {
  const f = duFrame(1, (sw, w, hh) => {
    let b = B(sw, 76, w - sw, 28, `background:${L.oat};border-bottom:1px solid ${L.sand}`) + T(sw + 16, 82, "Name", `color:${S}`) + T(sw + 330, 82, "Created", `color:${S}`) + T(sw + 470, 82, "Kind", `color:${S}`) + T(sw + 600, 82, "Clones", `color:${S}`);
    SNAPS.forEach((s, i) => { const ry = 104 + i * 30, sel = i === 2; b += (sel ? B(sw, ry, w - sw, 30, `background:${C.straw}33`) : "") + T(sw + 16, ry + 7, s[0].length > 34 ? s[0].slice(0, 33) + "~" : s[0]) + T(sw + 330, ry + 7, s[1]) + T(sw + 470, ry + 7, s[2]) + T(sw + 600, ry + 7, s[3] ? "1" : "-"); });
    b += B(sw, 264, w - sw, 44, `background:${L.cream};border-top:1px solid ${L.sand};border-bottom:1px solid ${L.sand}`) + pill(sw + 12, 274, 80, "Diff", "primary") + pill(sw + 100, 274, 80, "Clone...", "secondary") + pill(sw + 188, 274, 100, "Roll back...", "secondary") + pill(sw + 296, 274, 80, "Send...", "secondary") + pill(w - 118, 274, 100, "Delete...", "secondary");
    b += T(sw + 16, 320, "<b>before-fortune</b> compared with the live volume: 5 changes", X2.replace("26.667px", "13.333px").replace("32px", "16px"));
    [["A", C.matcha, "/Apps/fortune/fortune"], ["A", C.matcha, "/Users/milk/Documents/Plans 2026"], ["M", C.honey, "/Users/milk/README.txt"], ["D", DANGER, "/Users/milk/Documents/todo.txt"], ["R", C.blue, "/Users/milk/dd/a  to  /Users/milk/dd/sub/a2"]].forEach((c, i) => { const cy = 346 + i * 30; b += B(sw + 16, cy, 26, 22, `background:${c[1]}44;border:1px solid ${c[1]};border-radius:6px`) + TC(sw + 16, cy + 3, 26, `<b>${c[0]}</b>`) + T(sw + 54, cy + 3, c[2]); });
    b += label(sw + 16, 512, "A added  M changed  D deleted  R moved. Only changed parts are read, so it is fast.");
    b += TRi(w, w - 16, 556, "Open a changed file's folder in Files with Return", `color:${S}`);
    return b;
  }, { toolbar: btn(900 - 160, 38, 148, "Take snapshot", "primary") });
  return f.h + f.win();
}
function duRollback() {
  const shield = B(0, 0, 1024, 768, `background:rgba(58,46,39,.28)`);
  const sx = 232, sy = 190, sw2 = 560, sh = 320;
  const hdr = B(0, 0, sw2, 44, `background:${DANGER}`);
  const inner = T(16, 64, "Everything changed since before-fortune will be replaced.", X2.replace("26.667px", "13.333px").replace("32px", "16px")) +
    T(16, 92, "5 changes since then (2 added, 1 changed, 1 deleted,", `color:${S}`) + T(16, 110, "1 moved) will be undone. The volume goes back to", `color:${S}`) + T(16, 128, "how it was today at 13:02.", `color:${S}`) +
    T(16, 156, "<b>2 newer snapshots will be deleted too.</b>") + label(16, 176, "Use Send first if you want to keep a copy.") +
    B(16, 208, sw2 - 32, 50, `background:${C.honey}33;border:1px solid ${C.honey};border-radius:10px`, T(12, 8, "Apps that have files open on this volume") + T(12, 26, "should be closed first.") ) +
    B(0, sh - 56, sw2, 56, `border-top:1px solid ${L.sand}`) + btn(sw2 - 230, sh - 42, 100, "Cancel", "secondary") + btn(sw2 - 122, sh - 42, 106, "Roll back", "danger");
  return duSnapshots() + shield + sheet(sx, sy, sw2, sh, "Roll back to before-fortune?", inner, hdr);
}
function duClones() {
  const f = duFrame(2, (sw, w, hh) => {
    let b = T(sw + 24, 90, "Clones", X2 + BD) + label(sw + 24, 128, "A clone is a writable copy of a snapshot. Try something, then keep or discard it.");
    b += B(sw + 24, 160, w - sw - 48, 112, `background:${L.cream};border:1px solid ${L.sand};border-radius:14px`, icon(14, 14, 44, "folder", C.lav, C.esp) + T(72, 14, "<b>Milk-fortune</b>") + label(72, 36, "from the snapshot before-fortune, made Today 13:02") + label(72, 56, "Opens as /Volumes/Milk-fortune, uses 2 MB of its own") + pill(72, 80, 100, "Open in Files", "secondary") + pill(180, 80, 80, "Keep...", "primary") + pill(268, 80, 90, "Discard...", "secondary"));
    b += label(sw + 24, 296, "Keep makes the clone live and saves the current one as before-fortune.");
    b += label(sw + 24, 316, "Discard frees what the clone wrote. Neither can be undone, so both ask first.");
    return b;
  }, { toolbar: btn(900 - 150, 38, 138, "New clone...", "primary") });
  return f.h + f.win();
}
function duHealth() {
  const f = duFrame(3, (sw, w, hh) => {
    let b = T(sw + 24, 90, "Health", X2 + BD) + label(sw + 24, 128, "Scrub reads every block from the disk and checks it, only when the disk is idle.");
    b += B(sw + 24, 160, w - sw - 48, 150, `background:${L.cream};border:1px solid ${L.sand};border-radius:14px`, T(16, 12, "<b>Scrub</b>   running") + gauge(16, 42, w - sw - 80, 14, 0.42, C.straw) + label(16, 64, "42%   2.4 GB read of 5.7 GB   about 1 minute left   32 MB/s") + pill(16, 94, 80, "Stop", "danger") + T(120, 98, "Speed", `color:${S}`) + chip(180, 94, 62, "16", false) + chip(246, 94, 62, "32", true) + chip(312, 94, 62, "64", false) + T(386, 98, "MB/s", `color:${S}`));
    b += B(sw + 24, 328, w - sw - 48, 100, `background:${L.cream};border:1px solid ${L.sand};border-radius:14px`, T(16, 12, "<b>Schedule</b>") + label(16, 36, "Scrub automatically every") + chip(232, 32, 70, "12 h") + chip(308, 32, 70, "7 days", true) + chip(384, 32, 70, "Off") + label(16, 66, "Last full scrub: Sunday 03:12, no problems found."));
    b += B(sw + 24, 446, w - sw - 48, 112, `background:${L.cream};border:1px solid ${L.sand};border-radius:14px`, T(16, 12, "<b>Check the structure</b>") + label(16, 36, "Looks for mistakes in the volume's layout. Check changes nothing; Repair may.") + pill(16, 70, 90, "Check", "secondary") + pill(114, 70, 110, "Repair...", "secondary"));
    return b;
  }, { toolbar: btn(900 - 150, 38, 138, "Scrub now", "primary") });
  return f.h + f.win();
}
function duLocked() {
  const f = duFrame(4, (sw, w, hh) => "", {});
  const shield = B(0, 0, 1024, 768, `background:rgba(58,46,39,.28)`);
  const sx = 262, sy = 190, sw2 = 500, sh = 316;
  const inner = icon(16, 62, 44, "gear", C.latte, C.esp) + T(72, 62, "<b>Milk is encrypted and locked</b>") + label(72, 84, "Enter the passphrase to open it.") +
    T(16, 134, "Passphrase", `color:${S}`) + field(16, 154, sw2 - 32, "&#8226;&#8226;&#8226;&#8226;&#8226;&#8226;&#8226;&#8226;&#8226;&#8226;") + T(16, 196, "Lost it? Use the recovery key instead", `color:${S}`) +
    label(16, 224, "Three wrong tries and you wait a moment before trying again.") + B(0, sh - 56, sw2, 56, `border-top:1px solid ${L.sand}`) + btn(sw2 - 214, sh - 42, 100, "Cancel", "secondary") + btn(sw2 - 106, sh - 42, 90, "Unlock", "primary");
  return duSnapshots() + shield + sheet(sx, sy, sw2, sh, "Unlock Milk", inner);
}
function duSecurity() {
  const f = duFrame(4, (sw, w, hh) => {
    let b = T(sw + 24, 90, "Security", X2 + BD);
    b += B(sw + 24, 136, w - sw - 48, 150, `background:${C.matcha}22;border:1px solid ${C.matcha};border-radius:14px`, T(16, 12, "<b>&#10003; Encrypted and unlocked</b>") + label(16, 36, "Every block is encrypted with a key that only your passphrase (or the") + label(16, 56, "recovery key) can open. Looked at from outside, the disk is noise.") + pill(16, 90, 160, "Change passphrase...", "secondary") + B(184, 90, 90, 24, `border:1px solid ${L.sand};border-radius:12px`, TC(0, 4, 90, "<b>Lock now</b>", `color:${L.latte}`)) + label(290, 94, "needs a lock call (not available yet)"));
    b += B(sw + 24, 304, w - sw - 48, 126, `background:${L.cream};border:1px solid ${L.sand};border-radius:14px`, T(16, 12, "<b>Recovery key</b>") + label(16, 36, "It was shown once when the disk was made, as 8 groups of 4 digits.") + label(16, 56, "It can not be shown again. Keep it safe: it unlocks the volume if you forget."));
    b += B(sw + 24, 448, w - sw - 48, 110, `background:${L.cream};border:1px solid ${L.sand};border-radius:14px`, T(16, 12, "<b>Backups are encrypted</b>") + label(16, 36, "Send asks for a passphrase and encrypts the backup. You can choose") + label(16, 56, "Not encrypted when you send; then it is a plain copy of your files.") + pill(16, 80, 100, "Back up...", "secondary"));
    return b;
  }, { toolbar: "" });
  return f.h + f.win();
}
function backupTop() {
  return T(16, 62, "Snapshot", `color:${S}`) + field(16, 82, 300, "monday   Mon 09:00") + T(332, 62, "Changes since (optional)", `color:${S}`) + field(332, 82, 212, "tuesday") +
    T(16, 130, "Save to", `color:${S}`) + field(16, 150, 440, "/Volumes/Mac/backups/monday.stream") + pill(464, 152, 80, "Choose...", "secondary") + T(16, 196, "Protection", `color:${S}`);
}
function backupFoot(sw2, sh, send = "primary") {
  return B(0, sh - 56, sw2, 56, `border-top:1px solid ${L.sand}`) + btn(sw2 - 214, sh - 42, 100, "Cancel", "secondary") + (send === "primary" ? btn(sw2 - 106, sh - 42, 90, "Send", "primary") : off(sw2 - 106, sh - 42, 90, "Send"));
}
function duBackup() {
  const sx = 232, sy = 140, sw2 = 560, sh = 450;
  const inner = backupTop() + seg(16, 216, ["Encrypted", "Not encrypted"], 0, 130) +
    T(16, 262, "Passphrase", `color:${S}`) + field(16, 282, 250, dots(12)) + T(292, 262, "Again", `color:${S}`) + field(292, 282, 252, dots(12)) +
    T(16, 322, "&#10003; The passphrases match") + label(16, 344, "A backup has no recovery key. Write the passphrase down.") +
    backupFoot(sw2, sh);
  return duSnapshots() + veil() + sheet(sx, sy, sw2, sh, "Back up a snapshot", inner);
}
function duBackupPlain() {
  const sx = 232, sy = 140, sw2 = 560, sh = 450;
  const inner = backupTop() + seg(16, 216, ["Encrypted", "Not encrypted"], 1, 130) +
    B(16, 262, sw2 - 32, 62, `background:${C.honey}33;border:1px solid ${C.honey};border-radius:10px`, T(12, 8, "<b>This backup will not be encrypted.</b>") + T(12, 30, "Anyone who gets the file can read everything in it.", `color:${S}`)) +
    check(16, 342, false, "I understand") + backupFoot(sw2, sh, "off");
  return duSnapshots() + veil() + sheet(sx, sy, sw2, sh, "Back up a snapshot", inner);
}
function duRestore() {
  const sx = 232, sy = 170, sw2 = 560, sh = 380;
  const inner = T(16, 62, "Backup file", `color:${S}`) + field(16, 82, 440, "/Volumes/Mac/backups/monday.stream") + pill(464, 84, 80, "Choose...", "secondary") +
    T(16, 130, "Restore into", `color:${S}`) + field(16, 150, 300, "Milk (this volume)") + check(332, 154, false, "Replace existing") +
    T(16, 198, "Passphrase (only if the backup is encrypted)", `color:${S}`) + B(16, 218, 300, 28, `background:${L.cream};border:2px solid ${DANGER};border-radius:8px`, T(8, 5, dots(10))) +
    T(16, 256, "This backup needs its passphrase, or that was not it.", `color:${DANGER}`) + T(16, 274, "Check it and try again. Nothing was restored.", `color:${DANGER}`) +
    B(0, sh - 56, sw2, 56, `border-top:1px solid ${L.sand}`) + btn(sw2 - 214, sh - 42, 100, "Cancel", "secondary") + btn(sw2 - 106, sh - 42, 90, "Restore", "primary");
  return duSnapshots() + veil() + sheet(sx, sy, sw2, sh, "Restore from a backup", inner);
}
function duSpare() {
  const f = duFrame(0, (sw, w, hh) => {
    let b = T(sw + 24, 90, "vdb", X2 + BD) + label(sw + 24, 128, "4.0 GB, read-write, not a milkfs disk");
    b += B(sw + 24, 160, w - sw - 48, 116, `background:${L.cream};border:1px solid ${L.sand};border-radius:14px`, T(16, 12, "<b>Erase and format as milkfs</b>") + label(16, 36, "Makes an empty volume. Everything on the disk is erased.") + label(16, 56, "You can turn encryption on while you do it.") + pill(16, 84, 90, "Erase...", "secondary"));
    b += B(sw + 24, 294, w - sw - 48, 90, `background:${L.cream};border:1px solid ${L.sand};border-radius:14px`, T(16, 12, "<b>What is on it</b>") + label(16, 36, "milkOS can not read it. It may be a disk from another system."));
    b += label(sw + 24, hh - 60, "A disk that is in use can not be erased. The disk milkOS runs from is never offered.");
    return b;
  }, { sel: 6, tabs: false });
  return f.h + f.win();
}
function duFormat() {
  const sx = 232, sy = 150, sw2 = 560, sh = 420;
  const inner = T(16, 62, "Disk", `color:${S}`) + field(16, 82, 528, "vdb   4.0 GB   read-write") +
    T(16, 126, "Name", `color:${S}`) + field(16, 146, 300, "Spare") + label(332, 152, "Shown in the sidebar") +
    check(16, 194, true, "Encrypt this disk") + label(44, 216, "Only the passphrase or the recovery key opens it.") +
    T(16, 248, "Passphrase", `color:${S}`) + field(16, 268, 250, dots(12)) + T(292, 248, "Again", `color:${S}`) + field(292, 268, 252, dots(12)) +
    T(16, 306, "&#10003; The passphrases match") + label(16, 328, "A recovery key is shown next, once.") +
    B(0, sh - 56, sw2, 56, `border-top:1px solid ${L.sand}`) + btn(sw2 - 214, sh - 42, 100, "Cancel", "secondary") + btn(sw2 - 106, sh - 42, 90, "Continue...", "primary");
  return duSpare() + veil() + sheet(sx, sy, sw2, sh, "Erase and format vdb", inner);
}
function duFormatConfirm() {
  const sx = 232, sy = 210, sw2 = 560, sh = 270;
  const hdr = B(0, 0, sw2, 44, `background:${DANGER}`);
  const inner = T(16, 64, "Everything on vdb (4.0 GB) will be erased.", X2.replace("26.667px", "13.333px").replace("32px", "16px")) +
    label(16, 92, "It becomes an empty, encrypted milkfs volume called Spare.") + label(16, 112, "This can not be undone.") +
    T(16, 148, "&#10003; vdb is not in use, and it is not the disk milkOS runs from.") +
    B(0, sh - 56, sw2, 56, `border-top:1px solid ${L.sand}`) + btn(sw2 - 222, sh - 42, 100, "Cancel", "secondary") + btn(sw2 - 114, sh - 42, 98, "Erase vdb", "danger");
  return duSpare() + veil() + sheet(sx, sy, sw2, sh, "Erase vdb?", inner, hdr);
}
function duKey() {
  const sx = 232, sy = 150, sw2 = 560, sh = 420;
  const grp = (g, i, r) => T(82 + i * 100, 18 + r * 40, g, X2 + BD);
  const keys = [["4821", "0937", "5512", "6604"], ["3075", "1189", "2246", "9038"]];
  const inner = T(16, 62, "<b>This is the only time it is shown.</b>") + label(16, 84, "If you forget the passphrase, this key opens the disk Spare.") +
    B(16, 116, 528, 104, `background:${L.cream};border:2px solid ${C.honey};border-radius:12px`, keys.map((r, ri) => r.map((g, i) => grp(g, i, ri)).join("")).join("")) +
    pill(16, 232, 100, "Copy key", "secondary") +
    label(16, 276, "Keep it on paper or in a safe, not on this computer.") +
    check(16, 316, false, "I've saved it") +
    B(0, sh - 56, sw2, 56, `border-top:1px solid ${L.sand}`) + off(sw2 - 106, sh - 42, 90, "Done");
  return duSpare() + veil() + sheet(sx, sy, sw2, sh, "Save the recovery key for Spare", inner);
}
function duEncrypt() {
  const f = duFrame(4, (sw, w, hh) => {
    let b = T(sw + 24, 90, "Security", X2 + BD);
    b += B(sw + 24, 136, w - sw - 48, 172, `background:${L.cream};border:1px solid ${L.sand};border-radius:14px`, T(16, 12, "<b>Encrypting this volume</b>   running") + gauge(16, 42, w - sw - 80, 14, 0.24, C.straw) + label(16, 64, "24%   385 of 1486 blocks   now: your files") + label(16, 90, "You can keep using the disk. If it stops, or milkOS restarts,") + label(16, 108, "it carries on by itself.") + off(16, 132, 190, "Encrypt this volume..."));
    b += B(sw + 24, 326, w - sw - 48, 128, `background:${L.cream};border:1px solid ${L.sand};border-radius:14px`, T(16, 12, "<b>Not while it is encrypting</b>") + label(16, 36, "Change passphrase, Lock, Roll back, Clone, Keep, Scrub and Restore wait") + label(16, 54, "until it is done. Snapshots, Back up and Time Machine keep working.") + offPill(16, 84, 170, "Change passphrase..."));
    return b;
  }, { sel: 2, toolbar: "", kinds: { 2: "encrypting 24%" } });
  return f.h + f.win();
}

/* ============================== REMINDERS ============================== */
const LISTS = [["Personal", C.straw, 4], ["Home", C.matcha, 2], ["Work", C.blue, 1]];
const ITEMS = [
  ["Pick up bike", "Personal", C.straw, "Today 17:00", "", false, false],
  ["Call the dentist back", "Personal", C.straw, "Yesterday", "overdue", false, true],
  ["Water the plants", "Home", C.matcha, "Today", "weekly", false, false],
  ["Send the invoice", "Work", C.blue, "Fri 09:00", "", false, false],
  ["Buy milk", "Home", C.matcha, "Tomorrow", "", false, false],
  ["Book flights", "Personal", C.straw, "", "", true, false],
];
function remFrame(selList, inner, extra = "") {
  let h = base("Reminders") + dock([0]);
  const x = 130, y = 52, w = 780, hh = 620, sw = 188;
  let b = B(0, 32, sw, hh - 34, `background:${L.oat};border-right:1px solid ${L.sand}`) + searchBox(12, 42, sw - 24, "Search");
  [["Today", "3"], ["Scheduled", "5"], ["All", "7"]].forEach((s, i) => { const ry = 82 + i * 30; b += (i === selList ? B(8, ry - 3, sw - 16, 26, `background:${C.straw}33;border-radius:8px`) : "") + T(18, ry + 2, s[0]) + TRi(w, sw - 18, ry + 2, s[1], `color:${S}`); });
  b += label(16, 186, "LISTS");
  LISTS.forEach((l, i) => { const ry = 212 + i * 30; b += circle(18, ry + 2, 14, l[1]) + T(42, ry + 2, l[0]) + TRi(w, sw - 18, ry + 2, String(l[2]), `color:${S}`); });
  b += pill(12, hh - 64, sw - 24, "Add list", "secondary");
  b += inner(sw, w, hh);
  return h + appWin(x, y, w, hh, "Reminders", b) + extra;
}
const row = (x, y, w, it) => {
  const [title, list, col, due, rep, done, overdue] = it;
  return circle(x, y + 2, 22, done ? col : "transparent", `border:2px solid ${col}`) + (done ? TC(x, y + 5, 22, "&#10003;", `color:${INK};font-weight:700`) : "") +
    T(x + 36, y + 3, title, done ? `color:${S};text-decoration:line-through` : "") + (due ? T(x + 36, y + 24, `${due}${rep === "weekly" ? "   repeats weekly" : ""}`, `color:${overdue ? DANGER : S}`) : "") + TRi(w, w - 24, y + 3, list, `color:${S}`) + B(x, y + 52, w - 48, 1, `background:${L.sand}`);
};
function remList() {
  return remFrame(0, (sw, w, hh) => {
    let b = T(sw + 24, 90, "Today", X2 + BD) + label(sw + 24, 128, "3 reminders, 1 overdue");
    b += B(sw + 24, 156, w - sw - 48, 32, `background:${L.cream};border:1px solid ${L.sand};border-radius:16px`, T(14, 8, "+  Add a reminder   (try: pick up bike at 5pm)", `color:${S}`));
    [ITEMS[1], ITEMS[0], ITEMS[2]].forEach((it, i) => { b += row(sw + 28, 208 + i * 62, w - sw - 24, it); });
    b += T(sw + 24, 408, "COMPLETED", `color:${S}`) + TRi(w, w - 24, 408, "Hide", `color:${S}`) + row(sw + 28, 436, w - sw - 24, ITEMS[5]);
    b += label(sw + 24, hh - 56, "Return adds, Space completes, Delete removes, Cmd-N adds a list.");
    return b;
  });
}
function remAdd() {
  return remFrame(0, (sw, w, hh) => {
    let b = T(sw + 24, 90, "Today", X2 + BD) + label(sw + 24, 128, "3 reminders, 1 overdue");
    b += B(sw + 24, 156, w - sw - 48, 32, `background:${L.cream};border:2px solid ${C.straw};border-radius:16px`, T(14, 7, "Send the invoice friday 9am", "") + B(14 + 27 * 8, 7, 2, 16, `background:${L.esp}`));
    [ITEMS[1], ITEMS[0], ITEMS[2]].forEach((it, i) => { b += row(sw + 28, 280 + i * 62, w - sw - 24, it); });
    return b;
  }, (() => { const px = 130 + 188 + 24, py = 52 + 196; return shadow(px, py, 420, 70, 12) + B(px, py, 420, 70, `background:${L.milk};border:1px solid ${L.sand};border-radius:12px`, T(14, 10, "<b>Send the invoice</b>") + T(14, 32, "Fri 23 Oct, 09:00", `color:${S}`) + T(14, 50, "in list Work   alert at the time", `color:${S}`) + pill(420 - 110, 22, 94, "Add", "primary")); })());
}
function remAlert() {
  let h = base("Reminders") + dock([0]) + dim(0.1);
  const t = TOK.toast, tx = 1024 - t.right - 320;
  h += glass(tx, t.top, 320, 96, TOK.radius.toast, icon(12, 14, 44, "cal", L.milk, C.esp, `border:1px solid ${L.sand}`) + T(66, 12, "<b>Pick up bike</b>") + TRi(320, 308, 12, "now", `color:${S}`) + T(66, 32, "Personal, due 17:00", `color:${S}`) + pill(66, 60, 70, "Done", "primary") + pill(144, 60, 100, "Snooze 10m", "secondary") + pill(252, 60, 56, "Open", "secondary"));
  h += glass(48, 56, 300, 200, 20, T(16, 16, "REMINDERS", `color:${S}`) + T(16, 42, "<b>3 due today</b>", X2.replace("26.667px", "16px").replace("32px", "20px")) + T(16, 76, "Pick up bike   17:00") + T(16, 98, "Water the plants", "") + T(16, 120, "Call the dentist back", `color:${DANGER}`) + T(16, 150, "1 overdue", `color:${DANGER}`));
  h += T(300, 200, "A due reminder reaches you in the same three places as a Calendar alert:", `color:${C.esp};background:${C.milk}cc;border-radius:6px;padding:2px 6px`) + T(300, 224, "a toast with Done and Snooze, a card in the drawer, and a count on Today.", `color:${C.esp};background:${C.milk}cc;border-radius:6px;padding:2px 6px`);
  return h;
}

/* ------------------------------ page ------------------------------ */
function buildPage(SLIDES, meta) {
const N = SLIDES.length;
const id = (i) => `n${((i % N) + N) % N}`;
const svg = (s) => `<svg class="scene" viewBox="0 0 1024 768" role="img" aria-label="Mockup of ${s.name}"><foreignObject x="0" y="0" width="1024" height="768"><div class="stage">${s.draw()}</div></foreignObject></svg>`;
const rules = SLIDES.map((s, i) => `#r-${id(i)}:checked ~ .wrap .p-${id(i)}{display:grid}\n#r-${id(i)}:checked ~ .wrap .n-${id(i)}{display:flex}\n#r-${id(i)}:checked ~ .wrap .t-${id(i)}{border-color:var(--accent)}`).join("\n");

const html = `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>${meta.title}</title>
<style>
:root{--bg:#FBF6EC;--card:#FFFDF8;--line:#E2D5BD;--ink:#3A2E28;--mute:#7A6652;--accent:#C9684F;--chip:#F1E8D6;--easy:#4F8A5B;--med:#B8832A;--hard:#B5473B}
@media (prefers-color-scheme:dark){:root{--bg:#1E1915;--card:#2A231D;--line:#443A30;--ink:#F3EBDD;--mute:#B9A68C;--accent:#E9966F;--chip:#383028;--easy:#7FC08B;--med:#E2B359;--hard:#E8826F}}
*{box-sizing:border-box}html,body{margin:0}
body{background:var(--bg);color:var(--ink);font:16px/1.5 ui-sans-serif,system-ui,-apple-system,"Segoe UI",sans-serif}
.r{position:absolute;opacity:0;pointer-events:none}
.wrap{max-width:1400px;margin:0 auto;padding:20px 16px 48px}
header{display:flex;flex-wrap:wrap;gap:12px;align-items:baseline;justify-content:space-between;margin-bottom:16px}
h1{font-size:22px;margin:0}.sub{color:var(--mute);font-size:14px;max-width:780px}
.nav{display:none;gap:8px;align-items:center}
.btn{display:inline-block;color:var(--ink);background:var(--card);border:1px solid var(--line);border-radius:10px;padding:8px 16px;cursor:pointer;min-height:44px;line-height:26px;user-select:none}
.btn:hover{border-color:var(--accent)}.btn:focus-visible,.thumb:focus-visible{outline:3px solid var(--accent);outline-offset:2px}
.count{min-width:64px;text-align:center;color:var(--mute)}
.panel{display:none;grid-template-columns:minmax(0,1fr) 370px;gap:20px;align-items:start}
@media(max-width:980px){.panel{grid-template-columns:1fr}}
.stagebox{background:var(--card);border:1px solid var(--line);border-radius:14px;padding:12px}
.stagewrap{border-radius:6px;border:1px solid var(--line);overflow:hidden;line-height:0}
.scene{display:block;width:100%;height:auto}
.stage{width:1024px;height:768px;position:relative;overflow:hidden;font:13.333px/16px "Unifont","Menlo","SF Mono",ui-monospace,monospace;color:#3A2E28}
.stage div{box-sizing:border-box}
.cap{color:var(--mute);font-size:13px;margin-top:8px}
.notes{background:var(--card);border:1px solid var(--line);border-radius:14px;padding:18px}
.notes h2{margin:0 0 2px;font-size:20px}.notes .tag{color:var(--mute);margin:0 0 12px;font-size:14px}
.notes h3{font-size:12px;text-transform:uppercase;letter-spacing:.08em;color:var(--mute);margin:16px 0 6px}.notes p{margin:0}
.cost{display:inline-block;font-weight:700;border-radius:999px;padding:2px 12px;border:1.5px solid currentColor;font-size:14px}
.cost.easy{color:var(--easy)}.cost.medium{color:var(--med)}.cost.hard{color:var(--hard)}
.chips{display:flex;flex-wrap:wrap;gap:6px;padding:0;margin:0;list-style:none}.chips li{background:var(--chip);border-radius:8px;padding:2px 9px;font-size:14px}
.strip{display:grid;grid-template-columns:repeat(4,minmax(0,1fr));gap:10px;margin-top:20px}
@media(max-width:700px){.strip{grid-template-columns:repeat(2,minmax(0,1fr))}}
.thumb{display:block;cursor:pointer;border:2px solid var(--line);border-radius:10px;background:var(--card);padding:6px}
.thumb .tw{border-radius:4px;overflow:hidden;line-height:0}.thumb .tl{font-size:13px;margin-top:5px;padding:0 2px;display:flex;justify-content:space-between}
section.plan{margin-top:28px;background:var(--card);border:1px solid var(--line);border-radius:14px;padding:18px 20px}
section.plan h2{margin:0 0 8px;font-size:18px}
${rules}
</style></head><body>
${SLIDES.map((s, i) => `<input class="r" type="radio" name="s" id="r-${id(i)}"${i === 0 ? " checked" : ""}>`).join("")}
<div class="wrap">
<header><div><h1>${meta.h1}</h1><div class="sub">${meta.sub} Use Prev / Next or tap a thumbnail.</div></div>
${SLIDES.map((s, i) => `<div class="nav n-${id(i)}"><label class="btn" for="r-${id(i - 1)}" role="button" tabindex="0" aria-label="Previous">&larr; Prev</label><span class="count">${i + 1} / ${N}</span><label class="btn" for="r-${id(i + 1)}" role="button" tabindex="0" aria-label="Next">Next &rarr;</label></div>`).join("")}
</header>
${SLIDES.map((s, i) => `<article class="panel p-${id(i)}"><div class="stagebox"><div class="stagewrap">${svg(s)}</div><div class="cap">${s.n} ${s.name}: drawn 1:1 at 1024×768 and scaled to fit.</div></div>
<aside class="notes"><h2>${s.n}. ${s.name}</h2><p class="tag">${s.tag}</p><span class="cost ${s.cost}">${s.cost}</span>
<h3>When</h3><p>${s.fam}</p><h3>What you see</h3><p>${s.nw}</p><h3>Detail</h3><p>${s.helps}</p><h3>Build note</h3><p>${s.why}</p>
<h3>Main elements</h3><ul class="chips">${s.els.map((e) => `<li>${e}</li>`).join("")}</ul><h3>Font note</h3><p>${s.font}</p></aside></article>`).join("")}
<div class="strip">${SLIDES.map((s, i) => `<label class="thumb t-${id(i)}" for="r-${id(i)}" tabindex="0" role="button" aria-label="Show ${s.name}"><div class="tw">${svg(s)}</div><div class="tl"><b>${s.n}. ${s.name}</b><span>${s.cost}</span></div></label>`).join("")}</div>
<section class="plan"><h2>${meta.planTitle}</h2>${meta.plan}</section>
</div>
<script>addEventListener('keydown',function(e){var r=[].slice.call(document.querySelectorAll('.r')),c=r.findIndex(function(x){return x.checked});if(e.key==='ArrowRight')r[(c+1)%r.length].checked=true;else if(e.key==='ArrowLeft')r[(c-1+r.length)%r.length].checked=true;else if((e.key==='Enter'||e.key===' ')&&e.target.tagName==='LABEL'){e.preventDefault();e.target.click()}})</script>
</body></html>`;
return html;
}



const SLIDES = [
  { id: "du-over", n: "1", name: "Disk Utility: Overview", tag: "What is on the disk and is it safe", draw: duOverview, cost: "medium", fam: "A volume list on the left, a usage bar and facts on the right, like Disk Utility on other systems.", nw: "The overview speaks about safety, not just space: Time Machine's schedule, the last scrub result in words, and a warning when backups would be unencrypted.", helps: "Anyone who wants to know the disk is healthy and backed up.", why: "Reads /sys/fs/milkfs/info, auto and scrub. No kernel changes.", els: ["Volume list", "Usage bar", "Facts", "Time Machine card", "Health card"], font: "Numbers are the headline; keep labels short." },
  { id: "du-snaps", n: "2", name: "Disk Utility: Snapshots and Diff", tag: "Every past state, and what changed", draw: duSnapshots, cost: "medium", fam: "A table of snapshots with a toolbar of actions.", nw: "Diff shows exactly which files were added, changed, deleted or moved between a snapshot and now, using only the changed parts of the disk, so it is instant.", helps: "Answering \"what did that install change?\" and recovering from a mistake.", why: "snap, /sys/fs/milkfs/snapshots, diff, rollback, delete, clone.", els: ["Snapshot table", "Action bar", "Diff list with A/M/D/R chips"], font: "Paths are cut with ~ in the middle of long names." },
  { id: "du-roll", n: "3", name: "Disk Utility: Roll back (confirm sheet)", tag: "Destructive actions explain themselves", draw: duRollback, cost: "easy", fam: "A confirmation sheet with a red header.", nw: "It says what will be replaced in numbers (5 changes), says how many newer snapshots go too (a rollback deletes them) and to Send first, and names the button after the verb: Roll back, not OK.", helps: "Making a scary action safe.", why: "One sheet pattern reused by Roll back, Delete, Discard, Keep and Repair.", els: ["Red header", "Consequence line", "Newer snapshots go too", "Cancel / verb button"], font: "Danger button text is #FFFDF8 on #B5473B, 5.26:1." },
  { id: "du-clone", n: "4", name: "Disk Utility: Clones", tag: "Try it, then keep or discard", draw: duClones, cost: "easy", fam: "A card per clone with buttons.", nw: "Keep makes the clone live and saves the old state as a snapshot; Discard frees what the clone wrote. Both ask first.", helps: "Trying an app before installing it for real.", why: "clone, promote, discard.", els: ["Clone card", "Open in Files", "Keep / Discard"], font: "Plain verbs." },
  { id: "du-health", n: "5", name: "Disk Utility: Health", tag: "Scrub and structure check", draw: duHealth, cost: "medium", fam: "A progress bar, a schedule and a check button.", nw: "Scrub progress with a speed choice; errors would name the damaged file and every snapshot that holds it. Check never changes anything; Repair asks first.", helps: "Knowing nothing has silently rotted.", why: "/sys/fs/milkfs/scrub, check, repair.", els: ["Scrub card", "Speed chips", "Schedule chips", "Check / Repair"], font: "Short labels." },
  { id: "du-lock", n: "6", name: "Disk Utility: Unlock", tag: "A locked, encrypted volume", draw: duLocked, cost: "easy", fam: "A passphrase sheet.", nw: "Enter the passphrase or use the recovery key. Wrong tries are explained, not just refused.", helps: "Opening an encrypted disk.", why: "/sys/fs/milkfs/unlock (write the secret).", els: ["Passphrase field", "Recovery key link", "Unlock"], font: "Dots for the passphrase." },
  { id: "du-sec", n: "7", name: "Disk Utility: Security", tag: "Encryption and its limits, honestly", draw: duSecurity, cost: "easy", fam: "A status card, a change-passphrase button and a recovery key note.", nw: "Says what encryption protects and what it does not: a backup made with Send is a plain copy, in a honey warning. Lock now is drawn but disabled because the system has no lock call yet.", helps: "No false sense of safety.", why: "Reads the volume flags; change passphrase via /sys/fs/milkfs/passwd.", els: ["Encryption status", "Change passphrase", "Recovery key note", "Backup warning", "Lock (needs a call)"], font: "Body text only." },
  { id: "du-backup", n: "8", name: "Disk Utility: Back up", tag: "Encrypted by default, with a passphrase", draw: duBackup, cost: "medium", fam: "A save sheet.", nw: "A backup from an encrypted volume is encrypted by default: pick a snapshot, optionally only what changed since an older one, a destination on the Mac share, and a passphrase typed twice. A line says there is no recovery key for a backup.", helps: "Backups that people actually understand and can not leak by accident.", why: "snap send SNAP [--from OLDER] -o FILE, with the passphrase; receive is the matching Restore sheet.", els: ["Snapshot pickers", "Destination", "Protection: Encrypted / Not encrypted", "Passphrase twice", "Send"], font: "Fields are 28 px." },
  { id: "du-backup-plain", n: "9", name: "Disk Utility: Back up, not encrypted", tag: "A choice you have to mean", draw: duBackupPlain, cost: "easy", fam: "The same sheet with the other option picked.", nw: "Choosing Not encrypted removes the passphrase fields and shows a honey warning. Send stays off until you tick I understand.", helps: "A plain copy is sometimes right (a disk in a safe), never by accident.", why: "The same call without a passphrase.", els: ["Not encrypted selected", "Honey warning", "I understand", "Send off until ticked"], font: "Warning in body text." },
  { id: "du-restore", n: "10", name: "Disk Utility: Restore", tag: "A passphrase, only if the backup needs one", draw: duRestore, cost: "easy", fam: "An open sheet.", nw: "The passphrase field is optional. If the backup is encrypted and it is missing or wrong, the field gets a Danger border and the sheet says so in words, and nothing is restored.", helps: "No guessing whether a backup is encrypted.", why: "snap receive FILE [--to PATH] [--force] with an optional passphrase; EACCES means wrong or missing.", els: ["File", "Restore into", "Replace existing", "Passphrase", "Error line"], font: "Error in Danger text, never colour alone." },
  { id: "du-spare", n: "11", name: "Disk Utility: A spare disk", tag: "Disks the sidebar did not know before", draw: duSpare, cost: "easy", fam: "A page for a disk that is not mounted.", nw: "A new DISKS group in the sidebar lists locked and unformatted disks from /sys/fs/milkfs/disks. A disk milkOS can not read gets one card: Erase and format. The disk in use is never offered.", helps: "Finding the disk and knowing what you are about to touch.", why: "Reads /sys/fs/milkfs/disks.", els: ["DISKS group", "Erase card", "What is on it", "Rule line"], font: "Names are device names (vdb) or labels." },
  { id: "du-format", n: "12", name: "Disk Utility: Erase and format", tag: "Name it, encrypt it, set the passphrase", draw: duFormat, cost: "medium", fam: "A form sheet.", nw: "The disk is shown, not chosen. Name, an Encrypt checkbox (on), the passphrase twice and a word-based match line. Continue... is off until it is valid and leads to the confirm sheet.", helps: "Making a new disk without a terminal.", why: "snap mkfs DEV [LABEL] [--encrypt]: the passphrase on a second line, never echoed.", els: ["Disk (read-only)", "Name", "Encrypt checkbox", "Passphrase twice", "Match line", "Continue..."], font: "Dots for the passphrase." },
  { id: "du-format-confirm", n: "13", name: "Disk Utility: Erase confirm", tag: "The danger sheet names the disk and its size", draw: duFormatConfirm, cost: "easy", fam: "The same confirm pattern as Roll back.", nw: "Header in Danger red, the device and its size in the first sentence, what it becomes, and a line that vdb is not in use and not the disk milkOS runs from. Cancel is the default; the button says Erase vdb.", helps: "No mistaken disk.", why: "The confirm sheet again; the kernel refuses a mounted device (EBUSY).", els: ["Danger header", "Disk and size", "What it becomes", "Safety line", "Cancel / Erase vdb"], font: "Danger button text #FFFDF8 on #B5473B." },
  { id: "du-key", n: "14", name: "Disk Utility: Recovery key", tag: "Shown once, and you say you saved it", draw: duKey, cost: "easy", fam: "A one-time code sheet.", nw: "Eight groups of four digits at 2x in a honey-bordered box, a Copy key button, and an I've saved it checkbox that turns Done on. There is no Cancel and Esc does nothing.", helps: "A key nobody saw is a key nobody has.", why: "Shown from the mkfs reply; held in memory only until the sheet closes.", els: ["Key box", "Copy key", "I've saved it", "Done (off until ticked)"], font: "Key digits are monospace at 2x." },
  { id: "du-encrypt", n: "15", name: "Disk Utility: Encrypt this volume", tag: "A button, and a progress row that survives a restart", draw: duEncrypt, cost: "medium", fam: "A progress card.", nw: "On a plain volume the Security tab offers Encrypt this volume.... While it runs the card shows a progress bar, blocks done and the phase in words, the button is off, and the sidebar row says encrypting 24%. It carries on by itself after a restart.", helps: "Turning encryption on for a disk you already use.", why: "Progress comes from the conversion line in /sys/fs/milkfs/info; polled once a second while running.", els: ["Progress card", "Phase in words", "Encrypt button (off while running)", "Sidebar status", "What waits (Change passphrase, Lock, Roll back...)"], font: "Numbers in body text." },
  { id: "rem-list", n: "16", name: "Reminders: Today", tag: "A calm to-do list", draw: remList, cost: "medium", fam: "Smart lists (Today, Scheduled, All), your own coloured lists, a quick-add field and checkboxes.", nw: "Due dates are in words, overdue says \"Yesterday\" in red text (never colour alone), and what you type in the quick-add field is understood with the same words as the Calendar (today, tomorrow, friday, 5pm).", helps: "Remembering small things without a project plan.", why: "A text file per list and a small parser; the alarm daemon rings them.", els: ["Smart lists", "Lists", "Quick add", "Rows", "Completed group"], font: "Row title 16px, due line in secondary text." },
  { id: "rem-add", n: "17", name: "Reminders: Adding", tag: "Say it, see how it was understood", draw: remAdd, cost: "easy", fam: "Type a sentence, press Return.", nw: "A small preview shows how the sentence was understood (title, due, list, alert) before it is added, so nothing is guessed silently.", helps: "Fast entry with no form.", why: "The grammar already exists for cal add.", els: ["Quick-add field", "Parse preview"], font: "Preview in secondary text." },
  { id: "rem-alert", n: "18", name: "Reminders: Alert", tag: "Done and Snooze on the toast", draw: remAlert, cost: "easy", fam: "A notification with actions.", nw: "The toast carries Done and Snooze 10m, and the Today card shows how many are due and how many are overdue.", helps: "Dealing with a reminder without opening the app.", why: "Needs notification actions from the window system, and the alarm daemon.", els: ["Toast with actions", "Drawer card", "Today count"], font: "One line each." },
];

/* ================= SPEC-DISK-REMINDERS.md ================= */
const R2 = (a, b) => ratio(a, b).toFixed(2);
const spec = `# milkOS app specs: Disk Utility and Reminders

Generated by \`disk-reminders.mjs\` (contrast numbers computed from the tokens). Mockups: \`disk-reminders.html\`. Written for **the app engineer** (the apps) and **the window-system engineer** (anything touching notifications or the window API). Read \`SPEC.md\`, \`SPEC-APPS.md\` (Conventions for every app) and \`ART.md\` first. Only what exists today is assumed; what is missing is listed under "Needs".

---

## 1. Disk Utility
**Purpose:** see the disk, keep it safe, and undo mistakes: usage, Time Machine snapshots and their differences, clones, scrub and checks, encryption (including making a new encrypted disk and encrypting a volume in place), and backups to the Mac. **Everything comes from \`/sys/fs/milkfs/*\` and the \`snap\` tool (docs/filesystem.md).**
**Window:** 900 x 640 default, 680 x 480 minimum. **Sidebar** 188 wide (oat), two groups. **VOLUMES**: one row per mounted volume: the primary volume "Milk" (selected by default), its clones ("Milk-<name>", lavender folder icon), other mounted milkfs volumes, the Mac shared folder (read-only note), and the in-memory root. **DISKS**: every block device from /sys/fs/milkfs/disks that is *not* mounted: a locked or unmounted milkfs disk (shown by its label, kind "locked" or "not mounted") and any other disk (shown by its device name, kind "not formatted"). Each row is 48 high with 4 spare: a 32px icon, the name in bold, a one-line kind in secondary text, cut at 14 characters with ~ ("encrypting 24%" fits). While an in-place conversion runs the kind line is "encrypting 24%" (or "decrypting 24%"). **Toolbar** 44 under the title bar with the segmented control **Overview, Snapshots, Clones, Health, Security** and one primary button at the right that depends on the tab: Take snapshot (Overview, Snapshots), New clone... (Clones), Scrub now (Health), none (Security). Cmd-1 to Cmd-5 switch.

### Where each thing comes from
| In the window | Source |
|---|---|
| Size, used, free, snapshot and clone counts, format version | /sys/fs/milkfs/info |
| Snapshot list (name, time, clones) | /sys/fs/milkfs/snapshots, clones |
| Time Machine: on/off, interval, keep, daily, "next in 12m" | /sys/fs/milkfs/auto (write \`on\`, \`off\`, \`interval=15m\`, \`keep=K\`, \`daily=D\`, \`now\`) |
| Take snapshot | write a name to /sys/fs/milkfs/snapshot (or \`snap create NAME\`) |
| Diff | write \`A [B]\` to /sys/fs/milkfs/diff and read it back (\`snap diff A [B|live]\`): lines starting A, D, M, R |
| Roll back, Delete | write the name to /sys/fs/milkfs/rollback, /sys/fs/milkfs/delete |
| Clone, Keep (promote), Discard | /sys/fs/milkfs/clone (\`SNAP NAME\`), promote, discard |
| Scrub: status, start, stop, speed, schedule | /sys/fs/milkfs/scrub (read: status; write \`start\`, \`stop\`, \`rate=MB/s\`, \`auto=off|on|7d|12h\`) |
| Check, Repair | /sys/fs/milkfs/check (write \`check\` or \`repair\`, read the report) |
| Unlock, Change passphrase | write \`[dev=NAME ]SECRET\` to /sys/fs/milkfs/unlock (a locked disk in the DISKS group unlocks with its own device name); write \`OLD\\nNEW\` to /sys/fs/milkfs/passwd (wrong secret: EACCES) |
| Back up (send), Restore (receive) | \`snap send SNAP [--from OLDER] -o FILE\` (a backup from an encrypted volume is **encrypted by default**, with a passphrase), \`snap receive FILE [--to PATH] [--force]\` (with an optional passphrase; EACCES means wrong or missing) |
| Every disk: name, size, rw or ro, filesystem, plain or encrypted, mounted / locked / unmounted, label | /sys/fs/milkfs/disks, one line per device: \`NAME BYTES rw|ro milkfs plain|encrypted mounted=PATH|locked|unmounted LABEL\`, or \`NAME BYTES rw|ro other\` for anything milkOS can not read. Refresh with the other files. |
| Erase and format a disk | \`snap mkfs DEV [LABEL] [--encrypt]\` (writes /sys/fs/milkfs/mkfs). With --encrypt the passphrase goes on a second line and is never echoed; the recovery key is returned once; **the new volume is left locked**. A mounted device is refused with EBUSY. |
| Encrypt a volume in place | (coming) a button here; progress in /sys/fs/milkfs/info; resumable. See \"Encrypt this volume\" below. |
| Volume size and usage of the other volumes | statfs (not available for non-milkfs volumes yet: show only name and kind) |
Refresh every 2 seconds while the window is visible, every 1 second while a scrub or send is running, paused when the window is hidden.

### Overview
Name at 2x, one line of facts in secondary text ("milkfs, 8.0 GB, encrypted and unlocked"), a usage bar (24 high, radius 12, honey fill on oat), "1.2 GB used   6.8 GB free", four fact cards (Snapshots, Clones, Format, Last scrub; 64 high, cream, radius 12). Then a **Time Machine card** (cream, radius 14): "On   every 15 minutes   keeps the last 8 and one a day for 14 days", "Next snapshot in 12 minutes", buttons **Snapshot now** and **Settings** (a sheet: on/off switch, interval 1 min to 2 h, keep, daily; changes persist in the superblock). And a **health card**: matcha at 13% with a matcha border and "No problems found" when the last scrub was clean (Danger-styled when it found errors, listing the files). A **records meter** appears when 24 or more of the 30 snapshot and clone records are used ("26 of 30; the oldest automatic snapshot makes room; manual snapshots are never deleted").

### Snapshots
A table: Name, Created, Kind, Clones. Header 28, rows 30, selected row straw at 20%. **Kind** is derived from the name: \`auto-...\` is Auto (made by Time Machine), \`before-...\` is "Before install" (the safety snapshot made when a clone was kept), everything else is Manual. Actions in a 44 high bar under the table: **Diff** (primary, compares with live; a menu offers another snapshot), **Clone...**, **Roll back...**, **Send...**, **Delete...** (right-aligned). Roll back is disabled while the volume is being encrypted or decrypted (see Encrypt this volume). Delete is disabled for a snapshot that has a clone, with the reason "This snapshot has a clone; discard the clone first" (EBUSY). **Diff view** under the bar: a heading "before-fortune compared with the live volume: 5 changes" and a list with a 26px chip per change: **A** added (matcha), **M** changed (honey), **D** deleted (Danger), **R** moved (blue), then the path. Return opens the changed file's folder in Files. Only changed parts of the disk are read, so Diff is quick. **Restore from file...** (in the toolbar's menu) opens the receive sheet (see Back up).

### Clones
A card per clone (cream, radius 14): lavender icon, name, "from the snapshot before-fortune, made Today 13:02", "Opens as /Volumes/Milk-fortune, uses 2 MB of its own". Buttons **Open in Files**, **Keep...** (primary), **Discard...**. New clone... asks for a snapshot (default the newest) and a name. New clone and Keep (promote) are disabled while the volume is being encrypted or decrypted. Explanatory text: Keep makes the clone live and keeps the current state as the snapshot before-<name>; Discard frees what the clone wrote; neither can be undone.

### Health
**Scrub card:** state (idle, running), a progress bar (14 high, straw), "42%   2.4 GB read of 5.7 GB   about 1 minute left   32 MB/s", **Stop** (Danger pill) while running (Scrub now and the schedule are disabled while the volume is being encrypted or decrypted), and **Speed** chips (16, 32, 64 MB/s; default 32). **Schedule card:** "Scrub automatically every" with chips 12 h, 7 days (default), Off, and "Last full scrub: Sunday 03:12, no problems found." Errors found by a scrub are listed with the damaged file's path, the offset and every snapshot or clone that holds the block, in Danger text, with the advice to restore the file from an older snapshot (Versions). **Structure check card:** **Check** (changes nothing) and **Repair...** (asks first); the report appears in a monospace pane.

### Security
- **Encryption status card:** "Encrypted and unlocked" (matcha 13% fill) or "Not encrypted" (oat), a plain sentence ("Every block is encrypted with a key that only your passphrase or the recovery key can open"), buttons **Change passphrase...** and **Lock now**. **Lock now is drawn disabled** with "needs a lock call (not available yet)": the system can unlock but has no call to lock again.
- **Unlock sheet** (shown for a locked volume, 500 x 316): icon, "Milk is encrypted and locked", a passphrase field (dots), a link "Lost it? Use the recovery key instead" (the field then accepts the 8 groups of 4), "Three wrong tries and you wait a moment", buttons Cancel and **Unlock** (primary). A wrong secret says "That was not the passphrase" (never the system errno).
- **Change passphrase sheet:** current passphrase (or recovery key), new passphrase twice; no strength meter, just "At least 8 characters". It rewraps the key; no data is rewritten.
- **Recovery key card:** says it was shown once when the disk was made (as 8 groups of 4 digits) and can not be shown again.
- **Backups card** (cream): "Backups are encrypted. Send asks for a passphrase and encrypts the backup. You can choose Not encrypted when you send; then it is a plain copy of your files." **Back up...** button. (This replaces the old plaintext warning: encrypted is now the default.)
- **Encrypt this volume...** (a plain milkfs volume only; see below).

### Disks, Erase and format
**Disk page** (a row in DISKS that is not milkfs): the device name at 2x, one line ("4.0 GB, read-write, not a milkfs disk"), a card **Erase and format as milkfs** with a secondary **Erase...** button (never primary: it opens a sheet, it does not erase), and a card "What is on it" in plain words ("milkOS can not read it. It may be a disk from another system."). A locked milkfs disk's page has **Unlock...** (the Unlock sheet above) and Erase...; an unmounted unlocked milkfs disk shows its facts and Erase.... No tab bar and no toolbar button on these pages. A line at the bottom says "A disk that is in use can not be erased. The disk milkOS runs from is never offered."
**When Erase... is available:** the disk is **not mounted** (state unmounted or locked, including a locked encrypted disk, or an \`other\` disk) and is rw. Any mounted disk is always refused (EBUSY) and the home disk is always mounted, so the running disk never qualifies. Otherwise the button is **disabled with the reason beside it**: "Milk is in use (mounted at /)" for the disk milkOS runs from, "Spare is in use (mounted at /Volumes/Spare)", "This disk is read-only". The running disk's page never has an enabled Erase, whatever its state. The kernel refuses a mounted device with EBUSY as a backstop: if it happens, the sheet says "vdb became busy. Nothing was changed."
**Erase and format sheet** (560 x 420, cream header, not Danger: it is a form): **Disk** (a read-only field, "vdb   4.0 GB   read-write"; the disk is shown, never chosen here), **Name** (28 high field, default "Untitled"; **1 to 31 bytes of UTF-8**, so an accented letter counts 2; no "/", no control characters, can not start with "."; spaces and punctuation are fine; the same rule as the kernel and the Mac tool). The field stops accepting at 31 bytes and a counter "27 of 31" appears from 24 on; a rejected character is explained in words under the field ("A name can not start with a dot", "A name can not contain /"), never silently dropped, **Encrypt this disk** (checkbox, **on by default**, with "Only the passphrase or the recovery key opens it."), and when on, **Passphrase** and **Again** (dots, 28 high, side by side) with a match line under them in words: "&#10003; The passphrases match" or, when they differ, "The passphrases do not match" in Danger text (the words carry the meaning, not the colour). The passphrase needs at least 8 characters; no strength meter. A line "A recovery key is shown next, once." Buttons: **Cancel** and **Continue...** (primary; the ellipsis says another step follows), **Continue is off until the name is valid (1 to 31 bytes, rules above) and the two passphrases match**. Unchecking Encrypt hides the passphrase fields and the recovery-key line, and the button stays Continue....
**Erase confirm** (the same sheet pattern, 560 x 270): header **Danger #B5473B** "Erase vdb?"; first sentence names the disk and its size: "Everything on vdb (4.0 GB) will be erased."; then what it becomes: "It becomes an empty, encrypted milkfs volume called Spare." (or "empty milkfs volume" when not encrypted), "This can not be undone."; a line "&#10003; vdb is not in use, and it is not the disk milkOS runs from." (shown only when true; if it is not true the sheet can not be reached). **No safety-snapshot box** (the disk is being erased; there is nothing to keep). Buttons: **Cancel** (secondary, **the default**; Return and Esc cancel) and **Erase vdb** (Danger kind; the name of the disk is in the label so it can be read at a glance).
**After you confirm:** the sheet becomes a one-line progress ("Erasing vdb...") while the write runs. On success with encryption, the recovery key sheet opens; without encryption, the sheet closes and the new volume is selected in VOLUMES. On failure the sheet stays and shows the reason in words (EBUSY above) with the raw message under Details.
**Recovery key sheet** (560 x 420, cream header, title "Save the recovery key for Spare"): "**This is the only time it is shown.**" then "If you forget the passphrase, this key opens the disk Spare."; the key as **8 groups of 4 digits at 2x monospace, in two rows of four** on a cream box with a 2px honey border; a secondary **Copy key** button (only if the system has a clipboard; otherwise omit it); "Keep it on paper or in a safe, not on this computer."; a checkbox **I've saved it**; and one button, **Done**, which is **off until the box is ticked**. There is **no Cancel and Esc does nothing**, so the key can not be skipped by accident. The key is held in memory only until this sheet closes and is never written to a file, a log or the history. If the app is quit with this sheet open the key is lost, but the passphrase still opens the disk (the disk can be erased again); the sheet says so nowhere, it is a note for the engineer.
**The new disk is left locked** by the kernel. The app keeps the passphrase typed on the first sheet in memory just until **Done**, then unlocks the new volume once with it (writes \`dev=NAME SECRET\` to /sys/fs/milkfs/unlock), clears the passphrase and selects the volume, so the person who just chose the passphrase is not asked for it again. If the unlock fails, the Unlock sheet opens as normal.

### Encrypt this volume (in place)
A slot is reserved on the **Security** tab. When the selected volume is a plain milkfs volume, the status card says "Not encrypted" (oat) and has a secondary **Encrypt this volume...** button (not primary). It opens a sheet with **Passphrase** and **Again** (same rules, same match line), a honey note "Back up first if the files matter: Back up..." (a link that opens the Back up sheet), and **Cancel** / **Encrypt** (primary; this is not destructive, so no Danger header). Then the same **recovery key sheet** (title "Save the recovery key for <name>"), then the work starts.
**While it runs** (info reads \`encryption:  converting\` and a line \`conversion:  encrypt 24% (385/1486 blocks), live tree\`; the format is \`encrypt|decrypt PCT% (DONE/TOTAL blocks), PHASE[, paused: REASON]\`), the Security tab shows a card **Encrypting this volume   running** with a progress bar (14 high, straw on sand) at PCT, then "24%   385 of 1486 blocks   now: your files", then "You can keep using the disk. If it stops, or milkOS restarts, it carries on by itself." The **Encrypt this volume...** button is **drawn disabled**. Rules for the numbers: **DONE can be larger than TOTAL**, so when DONE is over TOTAL show only the percentage and the phase and drop the block counts; **PCT never reads 100 until it is done**, so the bar is never full while working; there is no "time left" (the kernel does not give one, and a guess from the rate would flip around). **PHASE in words:** \`live tree\` "your files", \`snapshot NAME\` "snapshot NAME", \`clone NAME\` "clone NAME", \`verifying\` "checking", \`clearing free space\` "clearing free space". The sidebar row's kind line and the Overview Format fact card say "encrypting 24%". Poll /sys/fs/milkfs/info once a second while it runs. When the conversion line disappears and \`encryption:\` reads \`on\`, the card becomes the usual "Encrypted and unlocked" card (matcha).
**Paused for space:** the line ends \`, paused: needs N more free KiB\`. The card says, in honey (a honey 13% fill, honey border, ink text) with the reason in words: "**Paused.** Encrypting needs 2 MB more free space. It carries on by itself when there is room." (N in KiB converted to MB or GB), and the bar stays where it is. Any other reason text is shown as the kernel gives it after "Paused:".
**It carries on by itself.** An interrupted conversion **resumes automatically at boot or when the volume is unlocked**, so there is no Resume button and no "stopped" state; a volume found part-way simply shows the running card.
**What waits while it converts** (the kernel refuses these with EBUSY; the app disables each one and says why, "Not while this volume is being encrypted", in text next to or under the control, never as a bare greyed button): **Change passphrase**, **Lock now**, **Roll back**, **Clone / New clone**, **Keep (promote)** a clone, **Restore from file** (receive), **Scrub now** and the scrub schedule, and a second **Encrypt** or **Decrypt**. **What keeps working:** reads and writes (the disk is in use as normal), **Take snapshot** and **Delete** snapshot, **Back up (Send)** (encrypted by default), and Time Machine. The same card with \`decrypt\` instead of \`encrypt\` reads "Decrypting this volume"; a button to start a decrypt is not part of this spec, but if one is added the progress card is ready for it.

### Back up and restore
**Back up sheet** (560 x 450): **Snapshot** picker, optional **Only what changed since** picker (incremental from an older one), **Save to** (default /Volumes/Mac/backups/<name>.stream, Choose... opens the Save panel), then **Protection**, a segmented control **Encrypted | Not encrypted**.
- **A backup from an encrypted volume defaults to Encrypted.** The sheet shows **Passphrase** and **Again** (dots), the match line ("&#10003; The passphrases match", or "The passphrases do not match" in Danger text), and a line "A backup has no recovery key. Write the passphrase down." Nothing is prefilled, not even the disk's own passphrase. **Send is off until the two match and are at least 8 characters.**
- **Not encrypted** removes the passphrase fields and shows the honey panel "This backup will not be encrypted. Anyone who gets the file can read everything in it." with a required checkbox **I understand**; Send stays off until it is ticked. It is a choice you have to make, never the default.
- **A backup from a plain (not encrypted) volume** has no Protection control: one quiet line, "This disk is not encrypted, so the backup is not either.", and Send is on.
Send shows a progress bar and the result; the shared folder is read-only unless the VM was started with \`MILK_SHARE_RW=1\`, in which case Send is disabled with "The Mac folder is read-only. Start milkOS with MILK_SHARE_RW=1 to back up to the Mac."
**Restore sheet** (560 x 380, title "Restore from a backup"): **Backup file** (field and Choose..., an Open panel), **Restore into** (the target volume, default the selected one) with a **Replace existing** checkbox (force; ticking it adds the Danger confirm below), and **Passphrase (only if the backup is encrypted)**, an optional dotted field that is empty by default. **Restore** is primary. The app tries with whatever is in the field; **EACCES means the passphrase is missing or wrong** and the kernel can not tell which, so the sheet says so too: the field gets a **2px Danger border** and two lines in Danger text, "This backup needs its passphrase, or that was not it." and "Check it and try again. Nothing was restored." The words carry the meaning, the border only supports them. Any other error is shown as a reason in words with the raw message under Details.

### The confirm sheet (every destructive action)
Roll back, Delete snapshot, Discard clone, Keep clone, Repair, Replace on receive, **Erase disk**, Forget (if added later). Sheet 560 wide, radius 14, window shadow, over a veil of ink at 28%. **Header 44 in Danger #B5473B** with the title as a question in #FFFDF8 ("Roll back to before-fortune?", 5.26:1). Body: one sentence of consequence in plain words with numbers ("5 changes since then, 2 added, 1 changed, 1 deleted, 1 moved, will be undone"), one sentence of what happens to the current state, and, where it is true, a checkbox **"Take a snapshot of the volume as it is now first"**, ticked by default (Keep clone, Repair). **Roll back and Replace have no such box:** a rollback deletes every newer snapshot (like zfs rollback -r), so a safety snapshot made first would be deleted with them. Their sheet instead states how many newer snapshots will be deleted and tells you to use Send first to keep a copy. A honey note appears when open files could be affected. Buttons right-aligned: **Cancel** (secondary, **the default**; Return and Esc cancel) and the verb button (**Danger** kind, named for the action: Roll back, Delete snapshot, Discard clone, Keep clone, Repair, Replace). The destructive button is never the default.

### Privilege and errors
Writing to /sys/fs/milkfs/* needs privilege. When the app runs as the ordinary user, the first destructive action asks for the user's password in a system panel (same look as the Open panel) and the app then acts through that authorisation for 5 minutes. Errors show the reason in words ("This snapshot has a clone", "Not enough room", "The volume is busy; close apps that use it") above the raw system message, which is available under Details.

### Contrast
Danger header text #FFFDF8 on #B5473B: ${R2("#FFFDF8", DANGER)}:1. Diff chips: text ink on the change colour at 27% over the surface: A ${R2(INK, mixHex(L.milk, L.matcha, 0.27))}:1, M ${R2(INK, mixHex(L.milk, L.honey, 0.27))}:1, R ${R2(INK, mixHex(L.milk, L.blue, 0.27))}:1, D (Danger tint) ${R2(INK, mixHex(L.milk, DANGER, 0.27))}:1. Danger text on milk (overdue, errors): ${R2(DANGER, L.milk)}:1; in the dark theme use #F0A0A0 (${R2("#F0A0A0", DARK.milk)}:1).

### Needs
- **App engineer:** the app, the confirm-sheet pattern (reusable), a diff list, polling of the sysfs files, the DISKS sidebar group, the Erase and format flow (two sheets and the recovery key sheet), the Encrypt this volume slot (sheet, progress card, paused state, the list of what waits). The backup and restore passphrase fields are already built to the interface.
- **Processes / security engineer:** a privilege model for writes to /sys/fs/milkfs (ask for the user's password once, then a short authorisation; mkfs and encrypt are the most sensitive writes, so they always ask if the 5-minute ticket has lapsed); a **lock** call (the volume can be unlocked but not locked again; Lock now stays drawn disabled until it exists, and during a conversion the reason is the conversion).
- **Filesystem engineer, answered:** label rule (1 to 31 bytes of UTF-8, no "/", no control characters, not starting with "."), the info conversion line and its phases, what is allowed during a conversion, automatic resume, backups have no recovery key, Erase works on any unmounted disk. All folded into the sections above. Still open: the exact wording of other \`paused:\` reasons if there are any besides free space.
- **Window-system engineer:** none beyond sheets, which already exist; a clipboard if Copy key is to be offered.
- **Not provided:** per-volume usage for non-milkfs volumes, a lock call. Encryption is now set up either when a disk is erased and formatted here, or on an existing volume with Encrypt this volume.

---

## 2. Reminders
**Purpose:** remember small things. Lists, due dates, repeats, and a gentle ring when they are due, rung by the existing alarm daemon.
**Window:** 780 x 620 default, 560 x 400 minimum. **Sidebar** 188 wide (oat): a search field (24 high), the smart lists **Today**, **Scheduled** and **All** with counts (rows 26 high, selected straw at 20%), then **LISTS**: each user list with a coloured dot and a count, and an **Add list** button (secondary pill) at the bottom. Lists take any accent colour. Default lists: Personal (straw), Home (matcha), Work (blue).
- **List view:** the list name at 2x, a line of counts in secondary text ("3 reminders, 1 overdue"), a **quick-add field** (32 high, radius 16, cream, 2px straw border while focused), then the rows. **Completed** is a collapsible group at the bottom (collapsed by default after 7 days of items; the Hide link toggles it).
- **Row:** a 22px checkbox circle with a 2px border in the list's **ink colour** (blue #4B63A8, straw #C9566B, matcha #5B8A5E, honey #A87812: non-text contrast ${R2(CH.blue, L.milk)}:1, ${R2(CH.straw, L.milk)}:1, ${R2(CH.matcha, L.milk)}:1, ${R2(CH.honey, L.milk)}:1 on milk, all at least 3:1; the plain accents are too pale), the title (primary text), under it the due line in secondary text, and on the right the list name in secondary text (shown in Today, Scheduled and All). A completed row has the circle filled with the plain accent and an **ink** check (ink on straw ${R2(INK, L.straw)}:1), and the title in secondary text with a line through it. **Overdue:** the due line starts with the word "Overdue" or the day ("Yesterday") and is in Danger text (${R2(DANGER, L.milk)}:1 on milk); the word carries the meaning, not the colour. Rows are 62 high with a 1px sand divider.
- **Smart lists:** Today = due today plus everything overdue (overdue first); Scheduled = every reminder with a due date, grouped by day; All = everything.
- **Quick add** understands the same words as the Calendar: a title followed by \`today\`, \`tomorrow\`, \`+3d\`, a weekday name or \`YYYY-MM-DD\`, optionally \`at 5pm\`, \`17:00\`, and \`every day|week|month|year\` (or \`every 2 weeks\`). A **preview line** under the field shows how the sentence was understood (title, due, repeat, list) before Return adds it, so nothing is guessed silently; Esc clears. Return with no date adds an undated reminder to the current list.
- **Editing:** clicking a row expands it in place (no dialog): Title, Notes, Date, Time (optional), Repeat (Never, Daily, Weekly, Monthly, Yearly, every N), List, and an **Alert** switch (on by default when there is a time; an all-day reminder alerts at 09:00 like the Calendar). Return saves, Esc cancels. Space completes the selected row, Delete removes it, with an **Undo** toast for 5 seconds ("Reminder deleted   Undo"). Rows can be dragged to reorder within a list (a manual order, kept), otherwise sorted by due date.
- **Repeating reminders:** completing one marks it done and creates the next one (date advanced by the rule); only one open copy exists at a time, so the list never fills with future copies. A monthly reminder on the 31st lands on the last day of shorter months, like the Calendar.
- **Alerts:** at the due time the alarm daemon posts a notification (a toast with **Done**, **Snooze 10m** and **Open**), a card in the notification drawer, and the Calendar-alert sound (Focus mutes the sound and the toast but keeps the card). **Snooze** adds a one-shot alert and does not change the due date. The **Today** card shows how many reminders are due and how many are overdue (and the Today card's calendar highlight uses an ink numeral).
- **Data:** \`~/Library/Reminders/<List>.rem\`, plain text, tab-separated like the Calendar and Clock files (one \`list\` line: name, colour; then one line per reminder: id, done, due date, due time or "-", repeat and every N, alert, title, notes, order). Lines that do not parse are skipped and counted, never fatal; writes go to a sibling file and are renamed into place. Local time only. The app engineer defines the exact format.
- **Sandbox:** its own folder (\`~/Library/Reminders\`) only.
- **Needs:** the alarm daemon reads the new files (one more file kind, like the Calendar's), notification actions for Done and Snooze (window-system engineer; stubbed until then, the toast is informational), the Today card hook (window-system engineer).
`;
const here = dirname(fileURLToPath(import.meta.url));
writeFileSync(join(here, "SPEC-DISK-REMINDERS.md"), spec);
writeFileSync(join(here, "disk-reminders.html"), buildPage(SLIDES, {
  title: "milkOS: Disk Utility and Reminders", h1: "milkOS: Disk Utility and Reminders",
  sub: "Fifteen screens for Disk Utility (a GUI over milkfs, including formatting a spare disk and encrypting in place) and three for Reminders (rung by the alarm daemon).",
  planTitle: "Design notes",
  plan: `<p><b>Disk Utility</b> uses only what milkfs already exposes under /sys/fs/milkfs and the snap tool. Every destructive action goes through one confirm sheet with a Danger header, a plain consequence line, an optional safety snapshot and a button named for the verb. <b>Reminders</b> keeps its data in plain text files under ~/Library/Reminders and reuses the Calendar's date words. Full spec: SPEC-DISK-REMINDERS.md.</p>`,
  file: "disk-reminders.html" }));
console.log("wrote disk-reminders.html and SPEC-DISK-REMINDERS.md");
