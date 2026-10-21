// Six more ideas for launching and switching apps, none of them a strip of icons at an edge. Run: node docs/ui-concepts/dock-alternatives-2.mjs
// Writes dock-alternatives-2.html. Importing v1.mjs re-writes milkos-1.0.html and SPEC.md with identical content.
import { B, T, TC, TR, TRi, icon, C } from "./scenes.mjs";
import { TOK, WP, glass, shadow as shadowLayers, menubar, filesWin, shellWin, dim } from "./v1.mjs";
import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const shadow = (x, y, w, h, r, layers = TOK.shadow.window) => shadowLayers(x, y, w, h, r, layers);
const circle = (x, y, d, bg, extra = "") => B(x, y, d, d, `background:${bg};border-radius:50%;${extra}`);
const APPS = { files: ["folder", C.honey, "Files"], term: ["term", C.esp, "Terminal"], web: ["globe", C.blue, "Browser"], notes: ["note", C.milk, "Notes"], paint: ["photo", C.milk, "Paint"], music: ["music", C.straw, "Music"], cal: ["cal", C.milk, "Calendar"], set: ["gear", C.latte, "Settings"], edit: ["doc", C.milk, "Editor"], send: ["folder", C.matcha, "Send to Mac"] };
const iconOf = (k, x, y, s) => { const a = APPS[k]; return icon(x, y, s, a[0], a[1], a[0] === "term" ? C.cream : C.esp, a[1] === C.milk ? `border:1px solid ${C.sand}` : ""); };
const milkDrop = (x, y, s) => B(x, y, s, s, `background:${C.milk};border:1px solid ${C.sand};border-radius:${Math.round(s * 0.28)}px`, circle(s * 0.28, s * 0.22, s * 0.44, C.milk, `border:${Math.max(2, s * 0.08)}px solid ${C.esp}`));
const tip = (x, y, t) => B(x, y, t.length * 8 + 20, 26, `background:rgba(255,253,248,.95);border:1px solid ${C.sand};border-radius:10px`) + T(x + 10, y + 5, t);
const ring = (x, y, s) => B(x - 3, y - 3, s + 6, s + 6, `border:2px solid ${C.straw};border-radius:${Math.round(s * 0.3)}px`);
const sep = (x, y, h) => B(x, y, 1, h, `background:${C.sand}`);
const cut = (t, n) => (t.length > n ? t.slice(0, n - 1) + "~" : t);


/* ---- 1. Rooms: each Space is a tile holding its own apps ---- */
function rooms() {
  let h = WP + menubar() + filesWin(110, 70, true) + shellWin(430, 270, false);
  const rooms = [["Work", "3 windows", ["files", "term", "notes"], true], ["Play", "1 window", ["music", "paint"], false], ["Away", "empty", [], false]];
  const tw = 196, gap = 8, w = 20 + rooms.length * tw + (rooms.length - 1) * gap + 56, x = (1024 - w) / 2, y = 686, hh = 70;
  let inner = "";
  rooms.forEach((r, i) => {
    const rx = 10 + i * (tw + gap);
    inner += B(rx, 8, tw, 54, `background:${r[3] ? C.straw + "33" : "rgba(255,253,248,.55)"};border:${r[3] ? 2 : 1}px solid ${r[3] ? C.straw : C.sand};border-radius:14px`);
    inner += T(rx + 12, 12, `<b>${r[0]}</b>`) + T(rx + tw - 12 - r[1].length * 8, 12, r[1], `color:${C.cocoa}`);
    r[2].forEach((k, j) => { inner += iconOf(k, rx + 12 + j * 30, 32, 22); });
    if (!r[2].length) inner += T(rx + 12, 32, "nothing open", `color:${C.latte}`);
  });
  const px = 10 + rooms.length * (tw + gap);
  inner += B(px, 8, 40, 54, `border:1.5px dashed ${C.latte};border-radius:14px`) + TC(px, 24, 40, "+", `color:${C.cocoa}`);
  h += glass(x, y, w, hh, 22, inner);
  h += tip(x + 10 + tw + gap + 20, y - 34, "Play   Music, Paint   Cmd-Opt-2");
  return h;
}

/* ---- 2. Recents strip: a history of what you touched, with a time axis ---- */
function recents() {
  let h = WP + menubar() + filesWin(110, 60, true) + shellWin(430, 250, false);
  const x = 30, y = 612, w = 964, hh = 142;
  let inner = "";
  // pinned apps, small, at the left
  ["files", "term", "web", "set"].forEach((k, i) => { inner += iconOf(k, 16, 14 + i * 29 - (i > 0 ? 0 : 0), 24); });
  inner += sep(54, 12, hh - 24);
  const cards = [["Documents", "folder", C.honey, "now", true], ["milk: sh", "term", C.esp, "2m", false], ["report.txt", "doc", C.milk, "14m", false], ["bike.png", "photo", C.milk, "1h", false], ["kernel", "folder", C.honey, "3h", false], ["ideas.md", "note", C.milk, "Mon", false]];
  cards.forEach((c, i) => {
    const cx = 68 + i * 136, op = i > 3 ? 0.62 : 1;
    inner += B(cx, 12, 126, 92, `opacity:${op};background:${c[4] ? C.straw + "33" : "rgba(255,253,248,.7)"};border:${c[4] ? 2 : 1}px solid ${c[4] ? C.straw : C.sand};border-radius:12px;overflow:hidden`,
      B(8, 8, 108, 52, `background:${c[1] === "term" ? C.esp : C.milk};border:1px solid ${C.sand};border-radius:6px;overflow:hidden`, c[1] === "term" ? "" : B(0, 0, 108, 8, `background:${C.cream}`) + B(0, 8, 24, 44, `background:${C.oat}`) + [0, 1, 2].map((k) => B(32 + k * 24, 18, 16, 16, `background:${c[2]}66;border-radius:4px`)).join("")) +
      icon(8, 66, 20, c[1], c[2], c[1] === "term" ? C.cream : C.esp, c[2] === C.milk ? `border:1px solid ${C.sand}` : "") + T(34, 68, cut(c[0], 11)));
    inner += TC(cx, 108, 126, c[3], `color:${C.cocoa};opacity:${op}`);
  });
  // time axis with snapshot ticks
  inner += B(68, 132, 820, 1, `background:${C.sand}`);
  [0, 2, 3, 5].forEach((i) => { inner += B(68 + i * 136 + 62, 128, 2, 9, `background:${C.cocoa}`); });
  inner += T(w - 16 - 11 * 8, 118, "4 snapshots", `color:${C.cocoa}`);
  h += glass(x, y, w, hh, 22, inner);
  h += tip(540, y - 34, "bike.png   opened 1h ago   Enter to open");
  return h;
}

/* ---- 3. Edge slots: eight fixed spots on the screen border ---- */
function edges() {
  let h = WP + menubar() + filesWin(110, 70, true) + shellWin(430, 270, false);
  const a = 0.9;
  const nub = (k, x, y, w, hh, r, is) => shadow(x, y, w, hh, 12, TOK.shadow.float) + B(x, y, w, hh, `background:rgba(255,253,248,${a});border:1px solid ${C.sand};border-radius:${r};overflow:hidden`, iconOf(k, (w - is) / 2, (hh - is) / 2, is));
  h += nub("files", 0, 28, 50, 50, "0 0 25px 0", 28) + nub("notes", 482, 28, 60, 26, "0 0 12px 12px", 20) + nub("cal", 974, 28, 50, 50, "0 0 0 25px", 28);
  h += nub("web", 0, 366, 26, 60, "0 12px 12px 0", 20) + nub("music", 998, 366, 26, 60, "12px 0 0 12px", 20);
  h += nub("set", 974, 718, 50, 50, "25px 0 0 0", 28) + nub("paint", 482, 742, 60, 26, "12px 12px 0 0", 20);
  // bottom-left slot is hovered: grown, labelled
  h += shadow(0, 702, 66, 66, 22, TOK.shadow.float) + B(0, 702, 66, 66, `background:${C.milk};border:2px solid ${C.straw};border-radius:0 22px 0 0`, iconOf("term", 10, 10, 44)) + circle(48, 706, 12, C.straw, `border:2px solid ${C.milk}`);
  h += tip(76, 722, "Terminal   1 window   corner = go");
  h += T(150, 190, "", "");
  return h;
}

/* ---- 4. Keypad: a fixed 3x3 grid that never reorders ---- */
function keypad() {
  let h = WP + menubar() + filesWin(110, 70, false) + shellWin(430, 270, true);
  const slots = ["files", "term", "web", "notes", "paint", "music", "cal", "set", null];
  const run = [0, 1, 3], s = 44, g = 8, pad = 12, w = pad * 2 + 3 * s + 2 * g, x = 1024 - w - 14, y = 768 - w - 14;
  let inner = "";
  slots.forEach((k, i) => {
    const cx = pad + (i % 3) * (s + g), cy = pad + Math.floor(i / 3) * (s + g);
    if (!k) { inner += B(cx, cy, s, s, `border:1.5px dashed ${C.latte};border-radius:13px`) + TC(cx, cy + 14, s, "+", `color:${C.cocoa}`); return; }
    inner += (i === 1 ? B(cx - 3, cy - 3, s + 6, s + 6, `background:${C.straw}44;border:2px solid ${C.straw};border-radius:16px`) : "") + iconOf(k, cx, cy, s);
    inner += B(cx + s - 14, cy + s - 16, 16, 18, `background:rgba(58,46,40,.72);border-radius:6px`) + T(cx + s - 11, cy + s - 15, String(i + 1), `color:${C.milk}`);
    if (run.includes(i)) inner += circle(cx + s - 8, cy - 5, 11, C.straw, `border:2px solid ${C.milk}`);
  });
  h += glass(x, y, w, w, 22, inner);
  h += tip(1024 - 14 - 236 - 24, y - 34, "2   Terminal   Cmd-2 or click");
  return h;
}

/* ---- 5. Trays: four labelled drawers, one open at a time ---- */
function trays() {
  let h = WP + menubar() + filesWin(110, 70, false) + shellWin(430, 270, true);
  const y = 714, hh = 42, labels = [["Make", false], ["Play", false], ["Tools", false]];
  const names = [["Make", 108, 0], ["Play", 88, 0], ["Tools", 96, 0]];
  const bx = 330;
  let inner = milkDrop(8, 6, 30), cx = 50;
  const pos = [];
  names.forEach((n, i) => { pos.push(cx); const open = i === 0; inner += B(cx, 5, n[1], 32, `background:${open ? C.straw + "55" : "rgba(255,253,248,.0)"};border:${open ? 2 : 1}px solid ${open ? C.straw : C.sand};border-radius:11px`, T(12, 7, `<b>${n[0]}</b>`) + T(n[1] - 24, 7, open ? "&#9662;" : "&#9652;", `color:${C.cocoa}`)); cx += n[1] + 8; });
  inner += sep(cx + 2, 8, 26);
  [0, 1, 3].forEach((k, i) => { inner += iconOf(["files", "term", "notes"][i], cx + 14 + i * 34, 7, 28) + B(cx + 14 + i * 34 + 11, 38, 6, 3, `background:${C.cocoa};border-radius:2px`); });
  const w = cx + 14 + 3 * 34 + 8;
  const x = (1024 - w) / 2;
  h += glass(x, y, w, hh, 21, inner);
  // open tray above Make
  const tx = x + pos[0] - 8, ty = 560, tw = 372, th = 138;
  let t = "";
  ["notes", "paint", "term", "edit"].forEach((k, i) => { const ix = 18 + i * 88; t += iconOf(k, ix, 18, 56) + TC(ix - 16, 80, 88, APPS[k][2].length > 9 ? APPS[k][2].slice(0, 8) + "~" : APPS[k][2]) + (k === "notes" ? circle(ix + 42, 12, 12, C.straw, `border:2px solid ${C.milk}`) : ""); });
  t += T(18, 108, "Make: write or draw   1-4 picks", `color:${C.cocoa}`);
  h += glass(tx, ty, tw, th, 20, t, { alpha: 0.9 });
  h += B(tx + 36, ty + th - 2, 20, 10, `background:rgba(255,253,248,.9);clip-path:polygon(0 0,100% 0,50% 100%)`);
  return h;
}

/* ---- 6. Selection-aware dock: the right side offers what fits the selection ---- */
function context() {
  let h = WP + menubar("Files") + filesWin(110, 70, true) + shellWin(480, 300, false);
  // selected file marker over the Files window
  h += B(598, 114, 76, 100, `background:${C.straw}33;border:2px solid ${C.straw};border-radius:12px`);
  const pin = ["files", "term", "web", "notes"], ctx = [["paint", "Paint"], ["web", "Browser"], ["send", "Send to Mac"]];
  const i = 56, g = 10, pad = 14;
  const left = pad * 2 + 0 + 40 + pin.length * (i + g);
  const w = left + 24 + ctx.length * (i + g) + 10;
  const x = (1024 - w) / 2, y = 662, hh = 88;
  let inner = milkDrop(pad, 16, 48);
  pin.forEach((k, j) => { inner += iconOf(k, pad + 48 + 14 + j * (i + g) - 0, 16, i - 8) + (j < 2 ? B(pad + 48 + 14 + j * (i + g) + 18, 68, 6, 3, `background:${C.cocoa};border-radius:2px`) : ""); });
  const cx0 = pad + 48 + 14 + pin.length * (i + g) + 6;
  inner += B(cx0 - 8, 8, 1, hh - 16, `background:${C.sand}`);
  inner += B(cx0, 6, ctx.length * (i + g) + 8, hh - 12, `background:${C.straw}22;border:1.5px dashed ${C.straw};border-radius:16px`);
  ctx.forEach((c, j) => { inner += iconOf(c[0], cx0 + 12 + j * (i + g), 16, i - 8) + (j === 0 ? "" : ""); });
  inner += TC(cx0, 68, ctx.length * (i + g) + 8, "for bike.png", `color:${C.cocoa}`);
  h += glass(x, y, w, hh, 26, inner);
  h += tip(x + cx0 + 6, y - 34, "Drop here, or press Enter, to open in Paint");
  return h;
}
const SLIDES = [
  { id: "rooms", n: "1", name: "Rooms", tag: "Each Space is a tile that holds its own apps", draw: rooms, cost: "medium", used: "70px of 768 tall (9%)", fam: "Spaces or virtual desktops, but shown as something you can read instead of a number.", nw: "The bar is not a list of apps. It is a list of places: Work has Files, Terminal and Notes, Play has Music and Paint. The lit room is the one you are in. Click another to go there, drop a window on a tile to send it, or press Cmd-Option and a number.", helps: "People who keep different jobs in different Spaces and lose track of which is which. It answers \"where did I leave that?\".", why: "A strip over data the compositor already has: the window list per Space. The new work is drawing the app icons for each Space and the drop target.", els: ["Room tiles", "Name and window count", "Small icons of what is open", "Lit current room", "+ new room", "Drop a window to move it"], font: "Names are short on purpose; counts use plain digits." },
  { id: "recents", n: "2", name: "Recents strip", tag: "The dock shows what you touched, not what you own", draw: recents, cost: "medium", used: "142px when open, shown on demand", fam: "Jump lists and Recent Items, made into the main surface.", nw: "Four pinned apps sit at the left. Everything else is a row of cards, newest first: folders, files and terminals, each with a little picture of the window and how long ago you used it. The line under the cards has a tick wherever milkfs took a snapshot, so you can see how far back it goes.", helps: "Anyone who thinks in files and projects, not in programs. \"Open the report I had open an hour ago\" is one click.", why: "Needs a short per-user history of opened paths and a small scaled picture saved with each one. The snapshot ticks come from the same list the Disk Utility shows. Shown by a key or by pushing at the bottom edge, so it costs nothing when hidden.", els: ["Pinned apps", "Cards, newest first", "Window picture", "Time label", "Snapshot ticks", "Fades after the fourth"], font: "Names cut at about 11 characters in Unifont." },
  { id: "edges", n: "3", name: "Edge slots", tag: "Eight fixed spots on the border of the screen", draw: edges, cost: "easy", used: "0 to 36px, only a small tab at each slot", fam: "Hot corners, but each corner holds one app instead of one action.", nw: "Four corners and four edge middles, each with a small tab. Push the pointer into the corner and the tab grows to show the app and its name. Click, or just keep pushing, to go there. A corner is the one place you cannot miss, however fast you move.", helps: "Mouse users who want a big target and a tiny interface. On a 4:3 screen the corners are the cheapest real estate there is.", why: "Hit-testing on the screen border, plus a grown tab drawn on top. Eight is a hard limit, which is the point: you pick your eight.", els: ["Corner tabs", "Edge-middle tabs", "Grown tab with name", "Running badge", "Settings: choose the eight"], font: "Only the grown tab has text." },
  { id: "keypad", n: "4", name: "Keypad", tag: "A fixed 3 by 3 grid that never reorders", draw: keypad, cost: "easy", used: "164px square in a corner", fam: "A phone keypad, or the number row you already use for Cmd-1 to Cmd-9.", nw: "Nine slots in a small square. Slot 5 is always Paint, slot 2 is always Terminal, so your hand learns the place. A number sits on each icon and is also the shortcut. Nothing slides around when you open or close something.", helps: "Keyboard people, and anyone whose dock keeps moving under the pointer. It trades flexibility for a memory your hand keeps.", why: "A fixed grid and a key map. Opens a window or focuses the one that exists. The smallest list of rules in the six.", els: ["3 by 3 grid", "Number on each slot", "Running badge", "Focused slot lit", "Empty slot (+)", "Cmd-1 to Cmd-9"], font: "One digit per slot." },
  { id: "trays", n: "5", name: "Trays", tag: "Four labelled drawers, one open at a time", draw: trays, cost: "easy", used: "42px closed, 180px with a tray open", fam: "A start menu with the clutter taken out, or folders on a shelf.", nw: "A short bar with the names Make, Play and Tools. Click one and a tray rises with bigger icons and full names, which is the only dock idea here where Unifont labels are always readable. The apps you have open sit at the right as ordinary icons.", helps: "Someone with more than ten apps who would rather read a name than guess an icon.", why: "A bar, a popover and a list stored in a settings file. You decide which tray an app lives in.", els: ["Tray buttons", "Open tray with labels", "Running apps", "Notch pointing at the button", "Number keys pick in a tray"], font: "Labels sit under icons, about 8 characters before cutting." },
  { id: "context", n: "6", name: "Selection-aware dock", tag: "The right side offers what fits what you have selected", draw: context, cost: "medium", used: "88px of 768 tall (11%)", fam: "Open With in the right-click menu, moved to where you can reach it.", nw: "The left is the dock as it is today. The right is a dashed zone that changes: select bike.png in Files and it offers Paint, Browser and Send to Mac. Drag the file onto one, or press Enter for the first. Select nothing and the zone is empty.", helps: "Everyone, and it makes the dock do something no other dock does: it knows what you are about to do.", why: "Needs the file manager to publish its selection and an app-to-type list. Without a type list, the zone shows recent apps. This is the most work of the six, but it needs no new idea about layout.", els: ["Pinned apps (as today)", "Dashed suggestion zone", "\"for bike.png\" label", "Drop target", "Enter opens the first"], font: "One short label; icons carry the rest." },
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
<h3>Familiar from</h3><p>${s.fam}</p><h3>What you see</h3><p>${s.nw}</p><h3>Detail</h3><p>${s.helps}</p><h3>Build note</h3><p>${s.why}</p>
<h3>Main elements</h3><ul class="chips">${s.els.map((e) => `<li>${e}</li>`).join("")}</ul><h3>Font note</h3><p>${s.font}</p></aside></article>`).join("")}
<div class="strip">${SLIDES.map((s, i) => `<label class="thumb t-${id(i)}" for="r-${id(i)}" tabindex="0" role="button" aria-label="Show ${s.name}"><div class="tw">${svg(s)}</div><div class="tl"><b>${s.n}. ${s.name}</b><span>${s.cost}</span></div></label>`).join("")}</div>
<section class="plan"><h2>${meta.planTitle}</h2>${meta.plan}</section>
</div>
<script>addEventListener('keydown',function(e){var r=[].slice.call(document.querySelectorAll('.r')),c=r.findIndex(function(x){return x.checked});if(e.key==='ArrowRight')r[(c+1)%r.length].checked=true;else if(e.key==='ArrowLeft')r[(c-1+r.length)%r.length].checked=true;else if((e.key==='Enter'||e.key===' ')&&e.target.tagName==='LABEL'){e.preventDefault();e.target.click()}})</script>
</body></html>`;
return html;
}



const here = dirname(fileURLToPath(import.meta.url));
writeFileSync(join(here, "dock-alternatives-2.html"), buildPage(SLIDES, {
  title: "milkOS: more dock ideas", h1: "milkOS: more dock ideas",
  sub: "Six ideas that are not a strip of icons: places, history, screen corners, a fixed keypad, labelled trays, and a dock that reads your selection.",
  planTitle: "Which ones I would build",
  plan: `<p><b>Most useful:</b> 6 (Selection-aware) and 2 (Recents). Both answer a question the dock never answers: what should I open next? 6 is the one that is new, and the right half can be built on its own while the left stays the dock you have.</p><p style="margin-top:8px"><b>Best with Spaces:</b> 1 (Rooms), if Spaces get used a lot. <b>Cheapest:</b> 3 (Edge slots) and 4 (Keypad), a day each, and they cost almost no screen. <b>Least needed:</b> 5 (Trays); it solves a problem only a long app list has.</p><p style="margin-top:8px"><b>My pick to try first:</b> 6, then 3 as a switch in Settings.</p>`,
  file: "dock-alternatives-2.html" }));
console.log("wrote dock-alternatives-2.html");
