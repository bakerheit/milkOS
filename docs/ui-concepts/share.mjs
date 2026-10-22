// Milk Drop sheet (Files > Share, send, receive, key-change) and Shield (parked, design only). Run: node docs/ui-concepts/share.mjs
// Writes share.html and shield.html. Importing v1.mjs re-writes milkos-1.0.html and SPEC.md with identical content.
import { B, T, TC, TR, TRi, icon, drop, C, X2, X3, BD } from "./scenes.mjs";
import { TOK, WP, glass, shadow as shadowLayers, menubar, dock, lightsX, dim } from "./v1.mjs";
import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const shadow = (x, y, w, h, r, layers = TOK.shadow.window) => shadowLayers(x, y, w, h, r, layers);
const INK = "#3A2E27";
const circle = (x, y, d, bg, extra = "") => B(x, y, d, d, `background:${bg};border-radius:50%;${extra}`);
// buttons exactly as in SPEC "Buttons"
const btnPill = (x, y, w, txt, kind = "primary") => B(x, y, w, 24, `background:${kind === "primary" ? C.straw : C.oat};border-radius:12px;${kind === "secondary" ? `border:1px solid ${C.sand}` : ""}`) + TC(x, y + 4, w, `<b>${txt}</b>`, `color:${INK}`);
const btn = (x, y, w, txt, kind = "primary", disabled = false) => {
  const fill = disabled ? C.oat : kind === "primary" ? C.straw : kind === "danger" ? "#B5473B" : C.oat;
  const fg = disabled ? C.latte : kind === "danger" ? C.milk : INK;
  return B(x, y, w, 28, `background:${fill};border-radius:8px;${kind === "secondary" || disabled ? `border:1px solid ${C.sand}` : ""}`) + TC(x, y + 6, w, `<b>${txt}</b>`, `color:${fg}`);
};
const parked = (x, y) => B(x, y, 150, 22, `background:${C.honey}55;border:1px dashed ${C.honey};border-radius:11px`) + TC(x, y + 3, 150, "parked, not scheduled", `color:${C.esp};font-size:12px;line-height:16px`);
const fp = (s) => s;

/* ---- Files window with the proposed toolbar, used behind the sheets ---- */
function filesToolbar(x = 60, y = 64, w = 700, hh = 470, opts = {}) {
  const t = TOK.window, sw = 168;
  let b = B(0, t.titleH, w, 40, `background:${C.cream};border-bottom:1px solid ${C.sand}`) +
    circle(12, 40, 24, C.oat) + TC(12, 44, 24, "&lt;") + circle(42, 40, 24, C.oat) + TC(42, 44, 24, "&gt;", `color:${C.latte}`) +
    B(78, 40, 300, 24, `background:${C.milk};border:1px solid ${C.sand};border-radius:12px`, T(12, 4, "Home &rsaquo; Documents")) +
    B(w - 300, 40, 150, 24, `background:${C.milk};border:1px solid ${C.sand};border-radius:12px`, T(12, 4, "Search", `color:${C.cocoa}`)) +
    btnPill(w - 138, 40, 70, "Share", opts.shareOn ? "primary" : "secondary") + circle(w - 60, 40, 24, C.oat) + TC(w - 60, 44, 24, "&#9776;");
  b += B(0, t.titleH + 40, sw, hh - t.titleH - 40 - 2, `background:${C.oat};border-right:1px solid ${C.sand}`) + T(16, 84, "FAVOURITES", `color:${C.cocoa}`);
  ["Home", "Documents", "Downloads", "Desktop", "Pictures", "Music"].forEach((f, i) => { b += (i === 1 ? B(8, 106 + i * 26 - 2, sw - 16, 24, `background:${C.straw}55;border-radius:8px`) : "") + T(18, 108 + i * 26, f); });
  b += T(16, 270, "NEARBY", `color:${C.cocoa}`) + circle(18, 296, 10, C.matcha) + T(36, 292, "alice") + circle(18, 322, 10, C.matcha) + T(36, 318, "bob");
  b += T(16, 356, "SHARED", `color:${C.cocoa}`) + T(18, 380, "Mac");
  [["doc", "ideas.md"], ["doc", "todo.txt"], ["photo", "bike.png"], ["folder", "school"]].forEach((f, i) => {
    const cx = sw + 28 + (i % 4) * 124, cy = 100, isF = f[0] === "folder", sel = i === 2;
    b += (sel ? B(cx - 8, cy - 8, 112, 112, `background:${C.straw}33;border:1px solid ${C.straw};border-radius:14px`) : "") + icon(cx + 16, cy, 56, f[0], isF ? C.honey + "55" : C.cream, isF ? C.cocoa : C.esp, isF ? "" : `border:1px solid ${C.sand}`) + TC(cx - 6, cy + 64, 100, f[1]);
  });
  b += B(0, hh - 26, w, 24, `background:${C.cream};border-top:1px solid ${C.sand}`) + T(sw + 12, hh - 22, "1 of 4 selected, 1.0 MB   212 GB available", `color:${C.cocoa}`);
  return shadow(x, y, w, hh, 12) + B(x, y, w, hh, `background:${C.milk};border-radius:12px;border:1px solid ${C.sand};overflow:hidden`, B(0, 0, w, t.titleH, `background:${C.cream};border-bottom:1px solid ${C.sand}`) + lightsX(13, 10) + TC(0, 8, w, "<b>Documents</b>") + b);
}
const base = (opts) => WP + menubar() + filesToolbar(60, 64, 700, 470, opts) + dock([0]);

/* ---- the device row used in the sheet ---- */
function deviceRow(y, w, d, sel = false) {
  const trust = { known: [C.matcha, "Known"], new: [C.honey, "New device"], changed: ["#B5473B", "KEY CHANGED"], off: [C.latte, "Not receiving"] }[d.trust];
  const dim_ = d.off ? 0.55 : 1;
  return B(12, y, w - 24, 64, `background:${sel ? C.straw + "33" : "rgba(255,253,248,.9)"};border:${sel ? 2 : 1}px solid ${sel ? C.straw : C.sand};border-radius:12px;opacity:${dim_}`,
    icon(10, 10, 44, "gear", d.tone, C.esp, `border:1px solid ${C.sand}`) + T(64, 8, `<b>${d.name}</b>`) + T(64, 28, `fp ${d.fp}`, `color:${C.cocoa}`) +
    B(w - 24 - 124, 12, 112, 24, `background:${trust[0]}${d.trust === "changed" ? "" : "44"};border-radius:12px`) + TC(w - 24 - 124, 16, 112, d.trust === "changed" ? `<b>${trust[1]}</b>` : trust[1], d.trust === "changed" ? `color:${C.milk};font-size:12px;line-height:16px` : `color:${INK};font-size:12px;line-height:16px`) +
    TRi(w - 24, w - 24 - 12, 40, d.off ? "open Milk Drop there" : d.recv, `color:${C.cocoa}`));
}
const sheetFrame = (x, y, w, hh, title, inner) => shadow(x, y, w, hh, 14) + B(x, y, w, hh, `background:${C.milk};border:1px solid ${C.sand};border-radius:14px;overflow:hidden`, B(0, 0, w, 44, `background:${C.cream};border-bottom:1px solid ${C.sand}`) + T(16, 14, `<b>${title}</b>`) + inner);

/* 1: nearby (scanning done) */
function nearby() {
  let h = base({ shareOn: true }) + dim(0.12);
  const x = 150, y = 138, w = 520, hh = 360, d = [
    { name: "alice", fp: "3fa2 91bc 07de 44a1", trust: "known", tone: C.blue, recv: "ready" },
    { name: "bob", fp: "9d10 22ee 5b7a 0c31", trust: "new", tone: C.lav, recv: "ready" },
    { name: "carol", fp: "71ac e03d 88f2 19b6", trust: "off", off: true, tone: C.latte, recv: "" }];
  let inner = TRi(w, w - 16, 14, "Esc", `color:${C.cocoa}`) + T(16, 56, "Sending bike.png (1.0 MB). Pick a device on this link.", `color:${C.cocoa}`);
  d.forEach((r, i) => { inner += deviceRow(84 + i * 76, w, r, i === 0); });
  inner += B(0, hh - 52, w, 52, `border-top:1px solid ${C.sand}`) + T(16, hh - 38, "Checked (SHA-256), not yet encrypted.", `color:${C.cocoa}`) + btn(w - 200, hh - 40, 88, "Cancel", "secondary") + btn(w - 104, hh - 40, 88, "Send", "primary");
  return h + sheetFrame(x, y, w, hh, "Milk Drop", inner);
}
/* 2: sending */
function sending() {
  let h = base({ shareOn: true }) + dim(0.12);
  const x = 150, y = 190, w = 520, hh = 240;
  let inner = icon(16, 60, 56, "photo", C.cream, C.esp, `border:1px solid ${C.sand}`) + T(84, 62, "<b>bike.png</b>   to alice") + T(84, 84, "416 KiB of 1024 KiB", `color:${C.cocoa}`) +
    B(84, 112, w - 100, 12, `background:${C.sand};border-radius:6px`, B(0, 0, (w - 100) * 0.4, 12, `background:${C.straw};border-radius:6px`)) + T(84, 136, "40%   about 2 s left", `color:${C.cocoa}`) +
    B(0, hh - 52, w, 52, `border-top:1px solid ${C.sand}`) + T(16, hh - 38, "alice accepted. Checking SHA-256 when done.", `color:${C.cocoa}`) + btn(w - 104, hh - 40, 88, "Cancel", "secondary");
  return h + sheetFrame(x, y, w, hh, "Sending to alice", inner);
}
/* 3: receiving prompt, new device (Powerbox look) */
function receiveNew() {
  let h = WP + menubar() + dock([0]) + dim(0.18);
  const x = 190, y = 150, w = 644, hh = 360;
  let inner = B(0, 44, w, 76, `background:${C.cream};border-bottom:1px solid ${C.sand}`) + icon(16, 56, 52, "photo", C.honey, C.esp) + T(84, 60, "<b>alice wants to send you bike.png</b>") + T(84, 82, "1.0 MB. This panel is milkOS's, not an app's.", `color:${C.cocoa}`);
  inner += B(16, 138, w - 32, 128, `background:${C.honey}33;border:1px solid ${C.honey};border-radius:12px`, T(16, 12, "<b>New device</b>   never seen before", `color:${C.esp}`) + T(16, 36, "Check this matches what alice sees on her screen:", `color:${C.cocoa}`) + TC(0, 62, w - 32, "3fa2  91bc  07de  44a1", X3.replace("40px", "32px").replace("48px", "40px") + BD) + T(16, 106, "Accepting trusts this device from now on.", `color:${C.cocoa}`));
  inner += B(0, hh - 60, w, 60, `border-top:1px solid ${C.sand}`) + T(16, hh - 42, "Saved to Downloads. Checked, not yet encrypted.", `color:${C.cocoa}`) + btn(w - 330, hh - 46, 100, "Decline", "secondary") + btn(w - 220, hh - 46, 204, "Trust alice and accept", "primary");
  return h + sheetFrame(x, y, w, hh, "Milk Drop: incoming", inner);
}
/* 4: key changed */
function keyChanged() {
  let h = base({ shareOn: true }) + dim(0.2);
  const x = 150, y = 150, w = 560, hh = 340;
  let inner = B(0, 44, w, 64, `background:#B5473B`) + circle(16, 54, 44, C.milk) + TC(16, 66, 44, "<b>!</b>", `color:#B5473B;font-size:26.667px;line-height:32px`) + T(72, 56, "<b>bob's key has changed</b>", `color:${C.milk}`) + T(72, 78, "Sending is blocked until you decide.", `color:${C.milk}`);
  inner += T(16, 118, "This usually means bob was reinstalled.", `color:${C.esp}`) + T(16, 136, "It could also mean someone is pretending to be bob.", `color:${C.esp}`) +
    B(16, 166, (w - 48) / 2, 70, `background:${C.oat};border-radius:10px`, T(12, 10, "Trusted before", `color:${C.cocoa}`) + T(12, 34, "<b>9d10 22ee 5b7a 0c31</b>")) +
    B(32 + (w - 48) / 2, 166, (w - 48) / 2, 70, `background:#B5473B22;border:1px solid #B5473B;border-radius:10px`, T(12, 10, "Now", `color:${C.cocoa}`) + T(12, 34, "<b>c4e8 0a95 6b12 f7d3</b>")) +
    T(16, 252, "Ask bob to read you his fingerprint, in person or by phone.", `color:${C.cocoa}`) +
    B(0, hh - 56, w, 56, `border-top:1px solid ${C.sand}`) + btn(w - 330, hh - 42, 100, "Cancel", "secondary") + btn(w - 220, hh - 42, 204, "Forget bob and re-verify", "danger");
  return h + sheetFrame(x, y, w, hh, "Milk Drop", inner);
}
/* 5: done toast + received */
function done() {
  let h = base({}) + glass(748, 40, 264, 76, TOK.radius.toast, icon(12, 14, 44, "photo", C.straw, C.esp) + T(66, 14, "<b>Received bike.png</b>") + T(66, 34, "from alice, in Downloads", `color:${C.cocoa}`) + btnPill(66, 48, 64, "Show") + btnPill(138, 48, 64, "Open", "secondary"));
  return h;
}
/* 6: empty / nothing found */
function empty() {
  let h = base({ shareOn: true }) + dim(0.12);
  const x = 190, y = 190, w = 440, hh = 250;
  let inner = circle(w / 2 - 36, 66, 72, C.oat, `border:2px dashed ${C.latte}`) + TC(w / 2 - 36, 90, 72, "?", `font-size:26.667px;line-height:32px;color:${C.cocoa}`) + TC(0, 150, w, "<b>No other milkOS machines found</b>") + TC(0, 172, w, "Milk Drop looks on the link to your other milkOS.", `color:${C.cocoa}`) +
    B(0, hh - 52, w, 52, `border-top:1px solid ${C.sand}`) + btn(16, hh - 40, 110, "Look again", "secondary") + btn(w - 104, hh - 40, 88, "Close", "primary");
  return h + sheetFrame(x, y, w, hh, "Milk Drop", inner);
}

/* ============ SHIELD (design only, parked) ============ */
const appWin = (x, y, w, hh, title, withShield, body = "") => shadow(x, y, w, hh, 12) + B(x, y, w, hh, `background:${C.milk};border:1px solid ${C.sand};border-radius:12px;overflow:hidden`,
  B(0, 0, w, 32, `background:${C.cream};border-bottom:1px solid ${C.sand}`) + lightsX(13, 10) + TC(0, 8, w, `<b>${title}</b>`) +
  (withShield ? B(w - 38, 4, 26, 24, `background:${C.matcha}55;border:1px solid ${C.matcha};border-radius:12px`, TC(0, 4, 26, "&#9660;", `color:${C.esp}`)) : "") + body);
const row = (y, w, label, val, tone) => T(18, y, label, `color:${C.cocoa}`) + TRi(w, w - 18, y, val, tone ? `color:${tone};${BD}` : BD);
/* S1: popover of a packaged app */
function shieldApp() {
  let h = WP + menubar("Moo2048") + appWin(80, 80, 560, 400, "moo2048", true, T(24, 60, "Score 2048", X2) + B(24, 110, 220, 220, `background:${C.oat};border-radius:14px`)) + dock([0]);
  const px = 380, py = 108, pw = 330, ph = 372;
  let inner = icon(16, 14, 40, "term", C.cream, C.esp, `border:1px solid ${C.sand}`) + T(68, 14, "<b>moo2048</b>") + T(68, 34, "installed from the Milk Store", `color:${C.cocoa}`) +
    B(14, 66, pw - 28, 1, `background:${C.sand}`) + T(18, 78, "WHAT THIS APP CAN DO", `color:${C.cocoa}`) +
    row(104, pw, "Your files", "Only moo2048's own folder") + row(128, pw, "Files you choose", "2 given via Open") +
    row(152, pw, "Network", "None", C.matcha) + row(176, pw, "Sound", "Yes") + row(200, pw, "Clipboard", "No") + row(224, pw, "Other windows", "No") +
    B(14, 252, pw - 28, 1, `background:${C.sand}`) + T(18, 262, "LAST 24 HOURS", `color:${C.cocoa}`) + T(18, 284, "&#10003; Opened save.dat   10:12", `color:${C.cocoa}`) + T(18, 304, "&#10003; Played a sound       10:12", `color:${C.cocoa}`) +
    btnPill(14, 332, 130, "See all activity", "secondary") + B(pw - 168, 332, 154, 24, `background:${C.oat};border:1px dashed ${C.latte};border-radius:12px`) + TC(pw - 168, 336, 154, "Network: change", `color:${C.latte}`) + parked(pw - 168, 362).replace("height:22px", "height:22px");
  h += glass(px, py, pw, ph, 18, inner, { alpha: 0.9 });
  return h;
}
/* S2: popover for a refused app */
function shieldRefused() {
  let h = WP + menubar("Peek") + appWin(80, 80, 520, 380, "peek", true, T(24, 60, "No file open", `color:${C.cocoa}`) + btnPill(24, 90, 120, "Open a file...")) + dock([0]);
  const px = 340, py = 108, pw = 330, ph = 330;
  let inner = icon(16, 14, 40, "doc", C.cream, C.esp, `border:1px solid ${C.sand}`) + T(68, 14, "<b>peek</b>") + T(68, 34, "sandboxed: sees only what you give it", `color:${C.cocoa}`) +
    B(14, 66, pw - 28, 1, `background:${C.sand}`) + row(78, pw, "Your files", "Nothing") + row(102, pw, "Files you choose", "1 given via Open") + row(126, pw, "Network", "None", C.matcha) + row(150, pw, "Sound", "No") +
    B(14, 180, pw - 28, 1, `background:${C.sand}`) + T(18, 190, "BLOCKED RECENTLY", `color:${C.cocoa}`) +
    B(12, 212, pw - 24, 56, `background:#B5473B1A;border:1px solid #B5473B;border-radius:10px`, T(12, 8, "<b>Refused: /System/Settings/passwd</b>") + T(12, 30, "tried to read it, 10:41 (audit log)", `color:${C.cocoa}`)) +
    T(18, 284, "Peek can not do this without asking you.", `color:${C.cocoa}`);
  h += glass(px, py, pw, ph, 18, inner, { alpha: 0.9 });
  return h;
}
/* S3: Settings > Privacy timeline */
function shieldTimeline() {
  let h = WP + menubar("Settings") + dock([0]);
  const x = 90, y = 60, w = 800, hh = 610;
  let body = B(0, 32, 180, hh - 34, `background:${C.oat};border-right:1px solid ${C.sand}`) + T(16, 46, "SETTINGS", `color:${C.cocoa}`);
  ["Appearance", "Wallpaper", "Display", "Dock", "Sound", "Time Machine", "Privacy", "About"].forEach((s, i) => { body += (s === "Privacy" ? B(8, 70 + i * 28 - 2, 164, 24, `background:${C.straw}55;border-radius:8px`) : "") + T(18, 72 + i * 28, s); });
  body += T(204, 52, "Privacy", X2) + T(206, 92, "What apps did, in plain words.", `color:${C.cocoa}`);
  body += B(204, 122, 572, 48, `background:${C.matcha}22;border:1px solid ${C.matcha};border-radius:12px`, T(16, 8, "<b>&#10003; Log is intact</b>") + T(16, 26, "4,213 records, chain verified 10:41", `color:${C.cocoa}`) + btnPill(572 - 140, 12, 124, "Verify again", "secondary"));
  ["All", "Refused", "Files", "Network", "Installs"].forEach((c, i) => { const on = i === 0; body += B(204 + i * 88, 184, 80, 26, `background:${on ? C.esp : "rgba(255,253,248,.85)"};border:1px solid ${on ? C.esp : C.sand};border-radius:13px`) + TC(204 + i * 88, 189, 80, c, `color:${on ? C.cream : C.esp}`); });
  const ev = [["10:41", "peek was refused", "/System/Settings/passwd", "#B5473B"], ["10:40", "You gave peek one file", "~/Documents/todo.txt", C.matcha], ["10:32", "moo2048 started", "sandboxed, no network", C.matcha], ["10:12", "fortune installed", "from the Milk Store", C.blue], ["10:05", "Milk Drop: received bike.png", "from alice, checked OK", C.matcha], ["09:58", "bob's key was trusted", "first use, fp 9d10 22ee...", C.honey]];
  ev.forEach((e, i) => { const ey = 226 + i * 56; body += circle(212, ey + 6, 12, e[3]) + (i < ev.length - 1 ? B(217, ey + 20, 2, 40, `background:${C.sand}`) : "") + T(240, ey, `<b>${e[1]}</b>`) + T(240, ey + 20, e[2], `color:${C.cocoa}`) + TRi(w, w - 24, ey, e[0], `color:${C.cocoa}`); });
  body += T(204, hh - 60, "Read-only view of the audit log. Nothing here changes what apps can do.", `color:${C.cocoa}`);
  h += appWin(x, y, w, hh, "Settings", false, body);
  return h;
}
/* S4: per-app permissions list */
function shieldApps() {
  let h = WP + menubar("Settings") + dock([0]);
  const x = 90, y = 60, w = 800, hh = 560;
  let body = B(0, 32, 180, hh - 34, `background:${C.oat};border-right:1px solid ${C.sand}`) + T(16, 46, "PRIVACY", `color:${C.cocoa}`) + B(8, 68, 164, 24, `background:${C.straw}55;border-radius:8px`) + T(18, 72, "Apps") + T(18, 100, "Activity") + T(18, 128, "Devices");
  body += T(204, 52, "Apps", X2) + T(206, 92, "What each app is allowed to do.", `color:${C.cocoa}`);
  const colX = { Files: 380, Network: 520, Sound: 630, Clipboard: 706 };
  body += B(204, 124, 572, 28, `background:${C.cream};border-bottom:1px solid ${C.sand}`) + T(216, 130, "App", `color:${C.cocoa}`) + Object.entries(colX).map(([n, cx]) => T(cx, 130, n, `color:${C.cocoa}`)).join("");
  const apps = [["Terminal", "term", C.esp, "Everything", "All", "Yes", "Yes"], ["Notes", "note", C.milk, "Documents", "None", "No", "No"], ["moo2048", "term", C.cream, "Own folder", "None", "Yes", "No"], ["peek", "doc", C.cream, "Chosen files", "None", "No", "No"], ["Browser", "globe", C.blue, "Downloads", "Web only", "Yes", "Yes"]];
  apps.forEach((a, i) => { const ay = 160 + i * 64; body += icon(214, ay + 6, 40, a[1], a[2], a[1] === "term" && a[2] === C.esp ? C.cream : C.esp, `border:1px solid ${C.sand}`) + T(266, ay + 16, `<b>${a[0]}</b>`) + T(colX.Files, ay + 16, a[3]) + T(colX.Network, ay + 16, a[4], a[4] === "None" ? `color:${C.matcha};${BD}` : "") + T(colX.Sound, ay + 16, a[5]) + T(colX.Clipboard, ay + 16, a[6]) + B(204, ay + 52, 572, 1, `background:${C.sand}`); });
  body += parked(204, hh - 66) + T(366, hh - 62, "Changing network access from here would come later.", `color:${C.cocoa}`);
  h += appWin(x, y, w, hh, "Settings", false, body);
  return h;
}


const SLIDES_SHARE = [
  { id: "nearby", n: "1", name: "Share: nearby devices", tag: "Files > Share opens the Milk Drop sheet", draw: nearby, cost: "medium", fam: "Select a file in Files and press Share (or drag it onto a name under NEARBY in the sidebar).", nw: "A sheet lists the milkOS machines on the link: name, fingerprint and trust (Known, New device, KEY CHANGED). Machines that are not receiving are greyed with the reason. Send is a filled straw button.", helps: "Reads `drop list` (name, address, fingerprint, receiving, trust). Scan takes about 1.5 s, so show a small spinner in the sheet header.", why: "A sheet with a list. Runs `MILK_TABLE=1 drop list` and re-runs it every few seconds while open.", els: ["Share button in the toolbar", "NEARBY sidebar group", "Device rows", "Trust chips", "Send / Cancel"], font: "Fingerprints are 19 characters in Unifont, fine at 8 px per letter." },
  { id: "sending", n: "2", name: "Sending", tag: "Progress and cancel", draw: sending, cost: "easy", fam: "The peer has accepted.", nw: "File chip, a straw progress bar and the numbers from the progress lines (40%, 416 KiB of 1024 KiB). Cancel stops the transfer.", helps: "Parses `drop: [####......]  40%  416 KiB / 1024 KiB`. Ends on `drop: sent ...` (exit 0) or `drop: <reason>` (exit 1), which closes the sheet with a toast or an error row.", why: "A progress bar bound to a parsed line. Cancel sends a signal (Ctrl-C equivalent).", els: ["File chip", "Progress bar", "Status line", "Cancel"], font: "Numbers in cocoa, one line." },
  { id: "incoming", n: "3", name: "Incoming, new device", tag: "A receive prompt in the Powerbox style", draw: receiveNew, cost: "medium", fam: "Same look as the Open panel: it is milkOS's panel, not an app's.", nw: "Names who is sending what and how big, and for a new device shows the fingerprint large to compare with the sender's screen. The main button says what it does: Trust alice and accept.", helps: "Fed by `drop recv`: it prints `drop: <who> wants to send you <file> (<size>)` then waits at `accept? [y/N]`. The sheet answers y or n on stdin. A known device shows a shorter version with Decline and Accept.", why: "A modal panel owned by the window server (like the Powerbox) plus a pipe to `drop recv`.", els: ["Sender and file", "Big fingerprint", "Trust warning panel", "Decline / Trust and accept", "Saved-to line"], font: "The fingerprint at 4x Unifont is the focal point." },
  { id: "changed", n: "4", name: "Key changed", tag: "The loud one", draw: keyChanged, cost: "easy", fam: "A warning dialog with a red header.", nw: "Both fingerprints side by side (trusted before, now), plain advice (ask in person or by phone) and one destructive action, Forget bob and re-verify. Sending is blocked until then.", helps: "Triggered by the boxed `WARNING: bob's KEY HAS CHANGED` on stderr (exit 1) or by trust = KEY CHANGED in the table. `drop forget bob` is the action.", why: "A dialog with two text blocks and a danger button.", els: ["Red header", "Two fingerprints", "Advice line", "Danger button"], font: "Danger red #B5473B with cream text is 5.26:1." },
  { id: "done", n: "5", name: "Received", tag: "A toast with next steps", draw: done, cost: "easy", fam: "A normal toast from the drawer's toast spec.", nw: "Received bike.png from alice, in Downloads, with Show (in Files) and Open buttons. The drawer keeps a card for it.", helps: "Finished files land in ~/Downloads. The audit log has user.drop.recv with result=ok.", why: "Existing toast with two pill buttons.", els: ["Toast", "Show / Open", "Drawer card"], font: "One line each." },
  { id: "empty", n: "6", name: "Nothing nearby", tag: "The empty state", draw: empty, cost: "easy", fam: "An empty list is not an error.", nw: "Says what Milk Drop looks for (other milkOS machines on the link) and offers Look again.", helps: "`drop list` returned no peers.", why: "One empty-state layout.", els: ["Empty state", "Look again", "Close"], font: "Question mark at 2x." },
];
const SLIDES_SHIELD = [
  { id: "app", n: "1", name: "Shield popover", tag: "What this app can do (read-only)", draw: shieldApp, cost: "medium", fam: "A small button in the title bar of a sandboxed app opens a panel, like a website's lock icon.", nw: "In plain English: files it can see, files you gave it, network, sound, clipboard, other windows, and the last 24 hours of its activity. Network: change is drawn but off.", helps: "Reads the process's capability set (docs/security.md) and `audit --app <exe>`.", why: "One popover, a list of rows and a query of the audit log. No new enforcement.", els: ["Title-bar shield button", "Capability rows", "Activity list", "See all activity", "Network change (parked)"], font: "Rows are short labels and values." },
  { id: "refused", n: "2", name: "A refused action", tag: "Show what was blocked", draw: shieldRefused, cost: "easy", fam: "The same panel for an app that tried something it was not allowed.", nw: "Shows the refusal (peek tried to read /System/Settings/passwd) in red with the time. Nothing to click: it was already blocked.", helps: "Built from `sandbox.deny` audit records.", why: "A filtered audit query.", els: ["Refused row", "Plain explanation"], font: "Red uses the danger colour." },
  { id: "timeline", n: "3", name: "Settings > Privacy", tag: "The audit log as a readable timeline", draw: shieldTimeline, cost: "medium", fam: "A Settings pane with filter chips and a timeline.", nw: "Starts with a green line: Log is intact (chain verified) and a Verify again button. Events read as sentences (peek was refused /System/Settings/passwd, You gave peek one file, bob's key was trusted).", helps: "From /proc/audit and `audit verify`. Every event type in docs/security.md maps to a sentence.", why: "A list renderer plus a table from event name to sentence.", els: ["Verified banner", "Filter chips", "Timeline", "Sentence table"], font: "Dots and the line carry the look." },
  { id: "apps", n: "4", name: "Settings > Privacy > Apps", tag: "Who can do what, in one table", draw: shieldApps, cost: "medium", fam: "A table, like Security & Privacy panes elsewhere.", nw: "One row per app: files, network, sound, clipboard. Green None is good news. Changing network access from here is parked.", helps: "Reads each installed app's grants.", why: "A table over the capability sets.", els: ["App table", "Per-column values", "Parked change controls"], font: "Fixed columns on the 8x16 grid." },
];

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

const here = dirname(fileURLToPath(import.meta.url));
writeFileSync(join(here, "share.html"), buildPage(SLIDES_SHARE, {
  title: "milkOS: Milk Drop sheet", h1: "milkOS: the Milk Drop sheet",
  sub: "AirDrop-style sharing between two milkOS machines, designed against the real `drop` commands (list, send, recv). Buttons follow the SPEC Buttons section.",
  planTitle: "Design notes",
  plan: "<p>Everything here uses commands that already exist and keep their output stable. The one honest thing the design says out loud: files are checked but not yet encrypted. When encryption lands, that line becomes Encrypted and the sheet does not change.</p>",
  file: "share.html" }));
writeFileSync(join(here, "shield.html"), buildPage(SLIDES_SHIELD, {
  title: "milkOS: Shield (design only, parked)", h1: "milkOS: Shield (design only, parked, not scheduled)",
  sub: "A read-only view of what each app can do and what it was refused. This is a design exploration only and is not scheduled for engineering. Any control that would change network access is drawn disabled and marked parked.",
  planTitle: "Status",
  plan: "<p><b>Parked, not scheduled.</b> Nothing here needs new enforcement: it reads the capability sets and the audit log that already exist. A first buildable version could be read-only. The network-changing controls overlap other work and stay parked.</p>",
  file: "shield.html" }));
console.log("wrote share.html and shield.html");
