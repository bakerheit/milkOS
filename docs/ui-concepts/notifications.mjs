// Notification drawer concepts on the milkOS 1.0 desktop with the "Tabs everywhere" window. Run: node docs/ui-concepts/notifications.mjs
// Writes notifications.html. Importing v1.mjs re-writes milkos-1.0.html and SPEC.md with identical content.
import { B, T, TC, TR, TRi, icon, drop, C, X2, BD } from "./scenes.mjs";
import { TOK, WP, glass, shadow as shadowLayers, menubar, dock, lightsX } from "./v1.mjs";
import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const shadow = (x, y, w, h, r, layers = TOK.shadow.window) => shadowLayers(x, y, w, h, r, layers);
const circle = (x, y, d, bg, extra = "") => B(x, y, d, d, `background:${bg};border-radius:50%;${extra}`);
const pill = (x, y, w, txt, on = false, h = 28, tone = "") => B(x, y, w, h, `background:${on ? C.esp : "rgba(255,253,248,.85)"};border:1px solid ${on ? C.esp : C.sand};border-radius:${h / 2}px`) + TC(x, y + (h - 16) / 2, w, txt, `color:${on ? C.cream : C.esp}`);
const badge = (x, y, n) => circle(x, y, 18, C.straw, `border:2px solid ${C.milk}`) + TC(x, y + 1, 18, `<b>${n}</b>`, `color:${C.milk};font-size:12px;line-height:14px`);

const NOTE = {
  mum: { ic: "photo", tone: C.straw, app: "Mail", title: "Mum", body: "Photos from the weekend", t: "10:38" },
  sam: { ic: "note", tone: C.blue, app: "Chat", title: "Sam", body: "Allocator merged, build is green", t: "10:31" },
  cal: { ic: "cal", tone: C.honey, app: "Calendar", title: "Dentist", body: "In 15 minutes, 12 Elm St", t: "10:30" },
  upd: { ic: "gear", tone: C.latte, app: "System", title: "Update ready", body: "milkOS 1.0.1, restart to install", t: "10:12" },
  bld: { ic: "term", tone: C.esp, app: "Shell", title: "cargo build", body: "passed, 0 warnings", t: "10:05" },
};
// one notification card, drawn inside a container of width w
const card = (x, y, w, n, { actions = false, compact = false, sel = false } = {}) => {
  const hh = compact ? 56 : actions ? 112 : 64;
  return B(x, y, w, hh, `background:${sel ? C.straw + "22" : "rgba(255,253,248,.82)"};border:1px solid ${sel ? C.straw : "rgba(255,255,255,.9)"};border-radius:14px`,
    icon(10, 10, 36, n.ic, n.tone, n.ic === "term" ? C.cream : C.esp) + T(56, 8, `<b>${n.title}</b>`) + TRi(w, w - 12, 8, n.t, `color:${C.cocoa}`) + T(56, 28, n.body.length > Math.floor((w - 70) / 8) ? n.body.slice(0, Math.floor((w - 70) / 8) - 1) + "~" : n.body, `color:${C.cocoa}`) +
    (actions ? B(56, 64, 64, 30, `background:${C.esp};border-radius:15px`) + TC(56, 71, 64, "Done", `color:${C.cream}`) + pill(126, 64, 96, "Later &#9662;", false, 30) + pill(228, 64, Math.max(60, w - 240), "Mute", false, 30) : ""));
};
const tabsWindow = (dimNotes = false, bellInBar = false) => {
  const x = 60, y = 70, w = 640, hh = 440;
  let h = shadow(x, y, w, hh, 12) + B(x, y, w, hh, `background:${C.milk};border-radius:12px;border:1px solid ${C.sand};overflow:hidden`,
    B(0, 0, w, 38, `background:${C.cream};border-bottom:1px solid ${C.sand}`) + lightsX(13, 12) +
    [["Documents", 1, 0], ["milk: sh", 0, 1], ["notes.md", 0, 0]].map((t, i) => { const tx = 84 + i * 150; return B(tx, 6, 144, 32, `background:${t[1] ? C.milk : "transparent"};border:${t[1] ? 1 : 0}px solid ${C.sand};border-bottom:0;border-radius:10px 10px 0 0`) + T(tx + 12, 11, t[1] ? `<b>${t[0]}</b>` : t[0], t[1] ? "" : `color:${C.cocoa}`) + (t[2] && bellInBar ? circle(tx + 116, 15, 10, C.straw) : ""); }).join("") +
    B(0, 38, 150, hh - 40, `background:${C.oat};border-right:1px solid ${C.sand}`) + T(16, 52, "FAVOURITES", `color:${C.cocoa}`) + B(8, 76, 134, 24, `background:${C.straw}55;border-radius:8px`) + T(18, 80, "Documents") + T(18, 108, "Downloads") + T(18, 136, "Desktop") + T(18, 164, "Pictures") +
    [["folder", "projects"], ["folder", "school"], ["doc", "todo.txt"], ["photo", "bike.png"], ["note", "ideas.md"], ["folder", "kernel"]].map((f, i) => {
      const cx = 170 + (i % 4) * 116, cy = 62 + Math.floor(i / 4) * 116, isF = f[0] === "folder";
      return icon(cx + 20, cy, 56, f[0], isF ? C.honey + "55" : C.cream, isF ? C.cocoa : C.esp, isF ? "" : `border:1px solid ${C.sand}`) + TC(cx - 6, cy + 64, 108, f[1]);
    }).join(""));
  return h;
};
const base = (extraDock = []) => WP + menubar() + tabsWindow();
const dockBadged = () => dock([0, 1]) + badge(484, 686, 3) + badge(548, 686, 1);

/* ---------- 1. Side drawer ---------- */
function side() {
  let h = base() + dockBadged();
  const x = 696, y = 36, w = 316, hh = 724;
  let inner = T(18, 14, "<b>Notifications</b>", X2.replace("26.667px", "16px") + "") + TRi(w, w - 18, 14, "Clear all", `color:${C.cocoa}`) +
    pill(14, 46, 64, "All", true, 26) + pill(84, 46, 96, "Mentions", false, 26) + pill(186, 46, 80, "Today", false, 26);
  inner += T(18, 88, "MESSAGES", `color:${C.cocoa}`) + TRi(w, w - 18, 88, "2", `color:${C.cocoa}`);
  inner += card(12, 110, w - 24, NOTE.mum) + card(12, 182, w - 24, NOTE.sam);
  inner += T(18, 262, "CALENDAR", `color:${C.cocoa}`) + card(12, 284, w - 24, NOTE.cal, { sel: true });
  inner += T(18, 364, "SYSTEM", `color:${C.cocoa}`) + card(12, 386, w - 24, NOTE.upd) + card(12, 458, w - 24, NOTE.bld, { compact: true });
  // today card
  inner += B(12, 536, w - 24, 110, `background:rgba(255,253,248,.82);border:1px solid rgba(255,255,255,.9);border-radius:14px`, T(14, 10, "TODAY", `color:${C.cocoa}`) + T(14, 32, "<b>2:30 PM</b>  Dentist") + T(14, 52, "5:00 PM  Pick up bike", `color:${C.cocoa}`) + T(14, 72, "7:30 PM  Dinner", `color:${C.cocoa}`));
  inner += B(12, hh - 62, w - 24, 44, `background:rgba(255,253,248,.82);border:1px solid rgba(255,255,255,.9);border-radius:14px`, T(14, 14, "Focus") + B(w - 90, 10, 44, 24, `background:${C.sand};border-radius:12px`, circle(2, 2, 20, C.milk)));
  h += glass(x, y, w, hh, 18, inner, { alpha: 0.78 });
  return h;
}

/* ---------- 2. Top shade ---------- */
function shade() {
  let h = base() + B(0, 0, 1024, 768, "background:rgba(58,46,40,.16)") + menubar();
  const x = 120, y = 28, w = 784, hh = 470;
  let inner = "";
  // left: quick settings
  const tile = (tx, ty, tw, label, sub, on) => B(tx, ty, tw, 58, `background:${on ? C.straw : "rgba(255,253,248,.78)"};border:1px solid ${on ? C.straw : C.sand};border-radius:16px`, T(14, 10, `<b>${label}</b>`, on ? `color:${C.milk}` : "") + T(14, 30, sub, on ? `color:${C.milk}` : `color:${C.cocoa}`));
  inner += T(18, 16, "QUICK SETTINGS", `color:${C.cocoa}`);
  inner += tile(14, 40, 168, "Wi-Fi", "home-5G", true) + tile(190, 40, 168, "Bluetooth", "off", false) + tile(14, 106, 168, "Focus", "Build", true) + tile(190, 106, 168, "Dark mode", "auto", false);
  const slider = (sy, label, v) => B(14, sy, 344, 58, "background:rgba(255,253,248,.78);border:1px solid " + C.sand + ";border-radius:16px", T(14, 8, `<b>${label}</b>`) + B(14, 36, 316, 8, "background:rgba(122,102,82,.25);border-radius:4px", B(0, 0, 316 * v, 8, `background:${C.esp};border-radius:4px`)));
  inner += slider(172, "Display", 0.68) + slider(238, "Sound", 0.4);
  inner += B(14, 308, 344, 74, "background:rgba(255,253,248,.78);border:1px solid " + C.sand + ";border-radius:16px", icon(12, 14, 46, "music", C.straw, C.esp) + T(68, 14, "<b>Cream Soda Skies</b>") + T(68, 34, "The Lactose Intolerants", `color:${C.cocoa}`) + T(260, 22, "&#9198;  &#9199;  &#9197;"));
  // divider
  inner += B(378, 16, 1, hh - 60, `background:${C.sand}`);
  // right: notifications
  inner += T(398, 16, "NOTIFICATIONS", `color:${C.cocoa}`) + TRi(w, w - 18, 16, "Clear all", `color:${C.cocoa}`);
  [NOTE.mum, NOTE.sam, NOTE.cal, NOTE.upd, NOTE.bld].forEach((n, i) => { inner += card(394, 40 + i * 70, w - 394 - 14, n, { sel: i === 2 }); });
  // pull handle
  inner += B(w / 2 - 28, hh - 18, 56, 6, `background:${C.latte};border-radius:3px`);
  h += glass(x, y - 20, w, hh + 20, 24, B(0, 20, w, hh, "", inner), { alpha: 0.8 });
  return h;
}

/* ---------- 3. Triage inbox ---------- */
function triage() {
  let h = base() + B(0, 0, 1024, 768, "background:rgba(58,46,40,.22)") + menubar();
  const x = 232, y = 50, w = 560, hh = 660;
  let inner = T(20, 16, "<b>Inbox</b>   5 left", `font-size:13.333px`) + TRi(w, w - 20, 16, "Esc to close", `color:${C.cocoa}`) +
    B(20, 44, w - 40, 8, `background:${C.sand};border-radius:4px`, B(0, 0, 90, 8, `background:${C.matcha};border-radius:4px`)) + T(20, 60, "You cleared 2 of 7. Triage, do not just dismiss.", `color:${C.cocoa}`);
  inner += pill(20, 90, 60, "All 5", true, 26) + pill(86, 90, 100, "milkOS 2", false, 26) + pill(192, 90, 112, "Personal 2", false, 26) + pill(310, 90, 96, "System 1", false, 26);
  inner += card(16, 128, w - 32, NOTE.sam, { actions: true, sel: true });
  // Later menu open on the first card
  inner += card(16, 252, w - 32, NOTE.mum, { actions: true });
  inner += card(16, 376, w - 32, NOTE.cal, { compact: true }) + card(16, 440, w - 32, NOTE.upd, { compact: true }) + card(16, 504, w - 32, NOTE.bld, { compact: true });
  inner += B(0, hh - 44, w, 44, `border-top:1px solid ${C.sand}`) + T(20, hh - 30, "Done = gone.  Later = back when you choose.  Mute = ask me less.", `color:${C.cocoa}`);
  h += glass(x, y, w, hh, 20, inner, { alpha: 0.86 });
  // the Later popover
  h += glass(x + 150, y + 166, 190, 156, 14, T(14, 12, "Remind me", `color:${C.cocoa}`) + T(14, 38, "In 1 hour") + T(14, 62, "<b>After this task</b>") + T(14, 86, "This evening") + T(14, 110, "Tomorrow 9:00"), { alpha: 0.95 });
  return h + dockBadged();
}

/* ---------- 4. Timeline tray ---------- */
function timeline() {
  let h = base();
  const x = 30, y = 410, w = 964, hh = 326;
  let inner = T(20, 16, "<b>Today</b>   the last 90 minutes") + pill(w - 440, 10, 80, "People", true, 26) + pill(w - 354, 10, 72, "Apps", false, 26) + pill(w - 276, 10, 80, "System", false, 26) + pill(w - 190, 10, 90, "Held (3)", false, 26) + TRi(w, w - 20, 16, "&#9662;", `color:${C.cocoa}`);
  // axis
  const ax = 40, aw = w - 80, ay = 168;
  inner += B(ax, ay, aw, 4, `background:${C.sand};border-radius:2px`);
  ["9:00", "9:30", "10:00", "10:30", "now"].forEach((t, i) => { const tx = ax + i * (aw / 4); inner += B(tx - 1, ay - 8, 3, 20, `background:${C.latte}`) + TC(tx - 30, ay + 18, 60, t, `color:${C.cocoa}`); });
  const place = [[NOTE.bld, 0.32, "above"], [NOTE.upd, 0.46, "below"], [NOTE.cal, 0.78, "above"], [NOTE.sam, 0.86, "below"], [NOTE.mum, 0.95, "above"]];
  place.forEach(([n, f, side]) => {
    const px = ax + f * aw, cw = 214, cx = Math.min(w - cw - 14, Math.max(14, px - cw / 2)), cy = side === "above" ? 50 : 206;
    inner += circle(px - 7, ay - 5, 14, n.tone, `border:3px solid ${C.milk}`) + (side === "above" ? B(px - 1, cy + 64, 2, ay - cy - 64 - 4, `background:${C.latte}`) : B(px - 1, ay + 8, 2, cy - ay - 8, `background:${C.latte}`)) + card(cx, cy, cw, n, { compact: true });
  });
  inner += T(20, hh - 26, "Scrub the line to jump back. Click a card to open where it came from.", `color:${C.cocoa}`);
  h += glass(x, y, w, hh, 20, inner, { alpha: 0.82 });
  return h + dock([0, 1]);
}

/* ---------- 5. Attached to windows ---------- */
function attached() {
  let h = WP + menubar() + tabsWindow(false, true);
  // title bar bell with badge
  const bx = 60 + 640 - 52, by = 70 + 7;
  h += B(bx, by, 36, 26, `background:${C.straw}33;border:1px solid ${C.straw};border-radius:13px`) + TC(bx, by + 5, 36, "&#9835;", `color:${C.esp}`) + badge(bx + 22, by - 6, 2);
  // popover scoped to this window
  const px = 420, py = 114, pw = 300, ph = 250;
  let inner = T(16, 12, "<b>From this window</b>") + T(16, 30, "milk: sh, Documents", `color:${C.cocoa}`) +
    card(10, 54, pw - 20, NOTE.bld, { compact: true, sel: true }) +
    card(10, 118, pw - 20, { ic: "folder", tone: C.honey, app: "Files", title: "Copy finished", body: "12 files to milk-ssd", t: "10:34" }, { compact: true }) +
    T(16, 190, "Other windows: 3 more", `color:${C.cocoa}`) + TRi(pw, pw - 16, 190, "Open inbox", `color:${C.straw}`) + B(0, 214, pw, 1, `background:${C.sand}`) + T(16, 226, "Mute this window", `color:${C.cocoa}`);
  h += glass(px, py, pw, ph, 16, inner, { alpha: 0.9 });
  // call-outs
  h += pill(60, 530, 236, "Dot on the tab it came from", false, 28) + pill(304, 530, 212, "Bell = this window only", false, 28) + pill(524, 530, 196, "Dock badges = apps", false, 28);
  h += T(60, 574, "Notifications live where they belong instead of in one pile.", `color:${C.esp}`) + T(60, 594, "Mum's email is in Mail. The build result is on the shell's tab.", `color:${C.esp}`);
  return h + dockBadged() + badge(812, 5, 5);
}

const SLIDES = [
  { id: "side", n: "1", name: "Side drawer", tag: "The familiar one: slides in from the right", draw: side, cost: "easy", fam: "Exactly what macOS, Windows and Android users expect: a panel from the right edge, notifications grouped by app, today's events, a Focus switch.", nw: "Groups are collapsible, the filter chips (All, Mentions, Today) are built in, and the selected notification can be acted on from the keyboard. Nothing surprising, which is the point.",
    helps: "Everyone. It is the safe default and the baseline to compare the others against.", why: "A glass panel, a list of cards and a group header. The only logic is grouping and a read/unread flag. Easiest of the five.",
    els: ["Edge drawer (glass)", "Grouped cards", "Filter chips", "Today card", "Focus", "Clear all / per group"], font: "Card text is cropped around 30 characters in Unifont; a proportional font would fit more." },
  { id: "shade", n: "2", name: "Top shade", tag: "A curtain from the menu bar with quick settings", draw: shade, cost: "medium", fam: "Phone-style pull-down: quick toggles and sliders on the left, notifications on the right, one gesture from the menu bar.", nw: "It merges notification centre and control centre into one place, wide enough to use on a desktop. Music controls sit with the toggles, and the shade covers the screen only as far as it needs to.",
    helps: "People who want one panel for everything, and anyone coming from a phone.", why: "Same pieces as the side drawer plus toggles and sliders. The extra cost is a pull-down animation (or just snap it open) and dimming the desktop behind it.",
    els: ["Menu bar pull-down", "Quick-setting tiles", "Sliders", "Media row", "Notification list", "Pull handle"], font: "Tile labels read well in Unifont. Sliders carry the look." },
  { id: "triage", n: "3", name: "Triage inbox", tag: "Notifications are a to-do list, not a pile", draw: triage, cost: "medium", fam: "A card list in a floating panel, like an email inbox.", nw: "Every notification has three honest actions: Done (gone), Later (comes back when you pick: in an hour, after this task, this evening) and Mute (ask me less). A progress bar shows you are clearing the list, and filter chips follow your Projects.",
    helps: "People who ignore notifications until they pile up, then dismiss them all and lose things.", why: "A timer queue for \"Later\" (a list of items with a wake-up time), plus per-app mute rules. The panel itself is the same cards with an action row.",
    els: ["Inbox panel", "Done / Later / Mute actions", "Later time picker", "Progress bar", "Project filter chips", "Mute rules"], font: "Action buttons are short words, which suits Unifont fine." },
  { id: "timeline", n: "4", name: "Timeline tray", tag: "Notifications placed on a time line", draw: timeline, cost: "medium", fam: "A tray that rises from the dock, like a Quick Look or a share sheet.", nw: "Notifications sit on a line of the last 90 minutes, so you can see when things happened and in what order, with chips for People, Apps, System and what Focus is holding back. Click a card to open where it came from.",
    helps: "People who think in time (\"what happened while I was in the build?\") and who lose the order of things in a flat list.", why: "A horizontal axis, dots and cards placed by time with a simple collision rule (alternate above and below). Scrubbing back through hours needs scrolling, which is a small amount of extra work.",
    els: ["Bottom tray", "Time axis", "Cards placed by time", "Type filter chips", "Held-by-Focus pile", "Scrub back through the day"], font: "Time labels at 8 px per character are fine. Cards need short titles." },
  { id: "attached", n: "5", name: "Attached to windows", tag: "Notifications live where they belong", draw: attached, cost: "medium", fam: "Dock badges and a bell, both things people already know.", nw: "Because windows can be tabs, a notification can sit on the exact tab it came from: the build result is a dot on the shell's tab, and the title-bar bell lists only that window's notifications. A global inbox still exists for everything else.",
    helps: "People with lots of windows: what needs you is visible in context, not collected in a drawer you have to open.", why: "Needs each notification to know its source window or tab, plus a small popover. The dots and bell are tiny shapes. It pairs naturally with \"Tabs everywhere\".",
    els: ["Dot on the source tab", "Title-bar bell with count", "Window-scoped popover", "Dock badges per app", "Global inbox link", "Mute this window"], font: "Badges use one or two digits only, so Unifont is fine." },
];

/* ------------------------------ page ------------------------------ */
const N = SLIDES.length;
const id = (i) => `n${((i % N) + N) % N}`;
const svg = (s) => `<svg class="scene" viewBox="0 0 1024 768" role="img" aria-label="Mockup of ${s.name}"><foreignObject x="0" y="0" width="1024" height="768"><div class="stage">${s.draw()}</div></foreignObject></svg>`;
const rules = SLIDES.map((s, i) => `#r-${id(i)}:checked ~ .wrap .p-${id(i)}{display:grid}\n#r-${id(i)}:checked ~ .wrap .n-${id(i)}{display:flex}\n#r-${id(i)}:checked ~ .wrap .t-${id(i)}{border-color:var(--accent)}`).join("\n");

const html = `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>milkOS: notification drawers</title>
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
.strip{display:grid;grid-template-columns:repeat(5,minmax(0,1fr));gap:10px;margin-top:20px}
@media(max-width:700px){.strip{grid-template-columns:repeat(2,minmax(0,1fr))}}
.thumb{display:block;cursor:pointer;border:2px solid var(--line);border-radius:10px;background:var(--card);padding:6px}
.thumb .tw{border-radius:4px;overflow:hidden;line-height:0}.thumb .tl{font-size:13px;margin-top:5px;padding:0 2px;display:flex;justify-content:space-between}
section.plan{margin-top:28px;background:var(--card);border:1px solid var(--line);border-radius:14px;padding:18px 20px}
section.plan h2{margin:0 0 8px;font-size:18px}
${rules}
</style></head><body>
${SLIDES.map((s, i) => `<input class="r" type="radio" name="s" id="r-${id(i)}"${i === 0 ? " checked" : ""}>`).join("")}
<div class="wrap">
<header><div><h1>milkOS: notification drawers</h1><div class="sub">Five ways to handle notifications on the milkOS 1.0 desktop, with the Tabs-everywhere window behind them. Use Prev / Next or tap a thumbnail.</div></div>
${SLIDES.map((s, i) => `<div class="nav n-${id(i)}"><label class="btn" for="r-${id(i - 1)}" role="button" tabindex="0" aria-label="Previous">&larr; Prev</label><span class="count">${i + 1} / ${N}</span><label class="btn" for="r-${id(i + 1)}" role="button" tabindex="0" aria-label="Next">Next &rarr;</label></div>`).join("")}
</header>
${SLIDES.map((s, i) => `<article class="panel p-${id(i)}"><div class="stagebox"><div class="stagewrap">${svg(s)}</div><div class="cap">${s.n} ${s.name}: drawn 1:1 at 1024×768 and scaled to fit.</div></div>
<aside class="notes"><h2>${s.n}. ${s.name}</h2><p class="tag">${s.tag}</p><span class="cost ${s.cost}">${s.cost}</span>
<h3>What feels familiar</h3><p>${s.fam}</p><h3>What is new</h3><p>${s.nw}</p><h3>Who it helps</h3><p>${s.helps}</p><h3>Cost in a kernel-drawn UI</h3><p>${s.why}</p>
<h3>Main elements</h3><ul class="chips">${s.els.map((e) => `<li>${e}</li>`).join("")}</ul><h3>Font note</h3><p>${s.font}</p></aside></article>`).join("")}
<div class="strip">${SLIDES.map((s, i) => `<label class="thumb t-${id(i)}" for="r-${id(i)}" tabindex="0" role="button" aria-label="Show ${s.name}"><div class="tw">${svg(s)}</div><div class="tl"><b>${s.n}. ${s.name}</b><span>${s.cost}</span></div></label>`).join("")}</div>
<section class="plan"><h2>Design notes</h2>
<p><b>Start with 1 Side drawer.</b> It is the familiar baseline and the cheapest, and everything else can build on it.</p>
<p style="margin-top:8px"><b>Then add the two ideas that change behaviour:</b> <b>3 Triage</b> (Done, Later, Mute instead of a pile) inside the drawer, and <b>5 Attached to windows</b> because it makes Tabs everywhere feel complete. <b>2 Top shade</b> is worth it if you also want a control centre. <b>4 Timeline</b> is the prettiest and the one I would build last.</p></section>
</div>
<script>addEventListener('keydown',function(e){var r=[].slice.call(document.querySelectorAll('.r')),c=r.findIndex(function(x){return x.checked});if(e.key==='ArrowRight')r[(c+1)%r.length].checked=true;else if(e.key==='ArrowLeft')r[(c-1+r.length)%r.length].checked=true;else if((e.key==='Enter'||e.key===' ')&&e.target.tagName==='LABEL'){e.preventDefault();e.target.click()}})</script>
</body></html>`;

const here = dirname(fileURLToPath(import.meta.url));
writeFileSync(join(here, "notifications.html"), html);
console.log("wrote notifications.html", html.length, "bytes");

export { NOTE, card, side, tabsWindow, dockBadged, badge, pill, circle };
