// Visual reference for ART.md: palette, icon family and colour map, sizes, wallpaper zones, boot splash.
// Run: node docs/ui-concepts/art-direction.mjs. The glyphs here are placeholders drawn from rectangles so the art lead can match the proportions.
import { B, T, TC, TR, TRi, icon, drop, C, X2, X3, X4, BD } from "./scenes.mjs";
import { TOK, WP, glass, shadow as shadowLayers, menubar, dock } from "./v1.mjs";
import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const circle = (x, y, d, bg, extra = "") => B(x, y, d, d, `background:${bg};border-radius:50%;${extra}`);
const INK = "#3A2E27";
// tile + glyph built from boxes, unit u = size/48
function tile(x, y, s, bg, kind, dark = false) {
  const u = s / 48, r = Math.round(s * 0.24);
  const accent = [C.honey, C.straw, C.matcha, C.blue, C.lav, C.latte].includes(bg);
  const fill = dark && !accent ? ({ [C.milk]: "#2E2520", [C.esp]: "#382D27" }[bg] || bg) : bg;
  const fg = accent ? INK : dark ? "#F4EBE0" : (bg === C.esp ? C.cream : C.esp);
  const R = (a, b, w, h, st = "") => B(a * u, b * u, w * u, h * u, st);
  let g = "";
  if (kind === "folder") g = R(9, 14, 14, 6, `background:${fg};border-radius:${3 * u}px ${3 * u}px 0 0`) + R(9, 19, 30, 18, `background:${fg};border-radius:${3 * u}px`);
  if (kind === "term") g = T(8 * u, 12 * u, "&gt;_", `font-weight:700;color:${fg};font-size:${Math.max(10, 20 * u)}px;line-height:${22 * u}px`);
  if (kind === "activity") g = R(9, 12, 30, 24, `border:${2.5 * u}px solid ${fg};border-radius:${4 * u}px`) + R(13, 28, 5, 3, `background:${fg}`) + R(18, 20, 5, 11, `background:${fg}`) + R(23, 24, 5, 7, `background:${fg}`) + R(14, 18, 20, 3, `background:${C.matcha};border-radius:2px`);
  if (kind === "globe") g = R(10, 10, 28, 28, `border:${3 * u}px solid ${fg};border-radius:50%`) + R(21, 10, 6, 28, `border:${2 * u}px solid ${fg};border-radius:50%`) + R(10, 23, 28, 2 * u, `background:${fg}`);
  if (kind === "notes") g = R(12, 9, 24, 30, `border:${2.5 * u}px solid ${fg};border-radius:${3 * u}px`) + R(12, 9, 24, 8, `background:${C.honey};border-radius:${3 * u}px ${3 * u}px 0 0`) + R(17, 24, 14, 2 * u, `background:${fg}`) + R(17, 30, 10, 2 * u, `background:${fg}`);
  if (kind === "editor") g = R(12, 9, 24, 30, `border:${2.5 * u}px solid ${fg};border-radius:${3 * u}px`) + R(17, 16, 14, 2 * u, `background:${fg}`) + R(17, 22, 14, 2 * u, `background:${fg}`) + R(17, 28, 6, 2 * u, `background:${fg}`) + R(24, 26, 3, 7, `background:${C.straw}`);
  if (kind === "paint") g = circle(13 * u, 13 * u, 10 * u, C.straw) + circle(26 * u, 13 * u, 10 * u, C.honey) + circle(13 * u, 26 * u, 10 * u, C.matcha) + circle(26 * u, 26 * u, 10 * u, C.blue);
  if (kind === "music") g = R(14, 28, 11, 9, `background:${fg};border-radius:50%`) + R(23, 11, 3, 23, `background:${fg}`) + R(23, 11, 13, 5, `background:${fg};border-radius:2px`);
  if (kind === "calendar") g = R(0, 0, 48, 14, `background:${C.straw}`) + TC(0, 17 * u, 48 * u, "<b>21</b>", `color:${INK};font-size:${Math.max(10, 22 * u)}px;line-height:${24 * u}px`);
  if (kind === "clock") g = R(10, 10, 28, 28, `border:${3 * u}px solid ${fg};border-radius:50%`) + R(23, 15, 2.5, 10, `background:${fg}`) + R(23, 23, 9, 2.5, `background:${fg}`);
  if (kind === "preview") g = R(9, 12, 30, 24, `border:${3 * u}px solid ${fg};border-radius:${4 * u}px`) + circle(16 * u, 17 * u, 8 * u, C.honey) + R(13, 28, 22, 5, `background:${C.matcha}`);
  if (kind === "gear") g = R(11, 11, 26, 26, `border:${5 * u}px solid ${fg};border-radius:50%`) + R(20, 20, 8, 8, `background:${fg};border-radius:50%`);
  if (kind === "store") g = R(10, 18, 28, 20, `background:${fg};border-radius:${4 * u}px`) + R(16, 10, 16, 14, `border:${3 * u}px solid ${fg};border-radius:${8 * u}px ${8 * u}px 0 0;border-bottom:0`);
  if (kind === "spaces") g = R(9, 11, 13, 11, `background:${fg};border-radius:3px`) + R(26, 11, 13, 11, `background:${fg};border-radius:3px`) + R(9, 26, 13, 11, `background:${fg};border-radius:3px`) + R(26, 26, 13, 11, `background:${fg};border-radius:3px`);
  const hl = B(0, 0, s, s * 0.34, "background:rgba(255,255,255,.14)");
  const shadowEl = B(x + 1, y + 3, s, s, `background:rgba(58,46,39,.10);border-radius:${r}px`);
  return shadowEl + B(x, y, s, s, `background:${fill};border-radius:${r}px;overflow:hidden;${fill === C.milk || fill === "#2E2520" ? `border:1px solid ${dark ? "#55463C" : C.sand}` : ""}`, hl + g);
}
const stageBg = (dark) => B(0, 0, 1024, 768, `background:${dark ? "#382D27" : C.cream}`);
const txt = (dark) => dark ? "#F4EBE0" : C.esp;
const sub = (dark) => dark ? "#BEA890" : "#6B5846";

/* 1: principles + palette */
function palette() {
  let h = stageBg(false) + T(48, 36, "Whole milk: soft, calm, warm, a little playful.", X2 + BD) + T(48, 80, "Flat, rounded, gently lit. One friendly pink for action. Mascots: the milk drop and the cow.", `color:#6B5846`);
  const toks = [["milk", "#FFFDF8"], ["cream", "#FBF6EC"], ["oat", "#EFE6D4"], ["sand", "#E2D5BD"], ["latte", "#C4AE8F"], ["cocoa", "#6B5846"], ["espresso", "#3A2E28"], ["ink", "#3A2E27"], ["straw", "#E58C97"], ["honey", "#EBB650"], ["matcha", "#9CBF9B"], ["blue", "#7F93C9"], ["lavender", "#B9A9D6"], ["peach", "#F2B79B"]];
  toks.forEach(([n, c], i) => { const x = 48 + (i % 7) * 134, y = 140 + Math.floor(i / 7) * 130; const light = ["#3A2E28", "#3A2E27", "#6B5846"].includes(c); h += B(x, y, 120, 76, `background:${c};border-radius:14px;border:1px solid ${C.sand}`) + T(x, y + 84, `<b>${n}</b>`) + T(x, y + 102, c, `color:#6B5846`); });
  h += T(48, 430, "Three tests for every asset", X2.replace("26.667px", "16px").replace("32px", "20px") + BD) +
    T(48, 462, "1  Squint: at half size and blurred it is still recognisable and still milkOS.") + T(48, 484, "2  Neighbour: next to its siblings it has the same radius, light and line weight.") + T(48, 506, "3  Both themes: it works on the light and the dark (Chocolate milk) theme.");
  h += T(48, 560, "Never", X2.replace("26.667px", "16px").replace("32px", "20px") + BD) + T(48, 592, "pure black or white, neon or saturated colours outside the tokens, hard bevels,", `color:#6B5846`) + T(48, 612, "text inside icons (except the Calendar date), gradients that fight the flat look.", `color:#6B5846`);
  return h;
}
/* 2: icon family */
function family(dark = false) {
  let h = stageBg(dark) + T(40, 28, `Icon family${dark ? " (dark theme)" : ""}`, X2 + BD + `color:${txt(dark)}`) + T(40, 70, "Tile radius 24%. Glyph 55 to 60% of the tile, one accent. Ink glyph on every accent tile in both themes.", `color:${sub(dark)}`);
  const apps = [["Files", C.honey, "folder"], ["Terminal", C.esp, "term"], ["Activity Monitor", C.esp, "activity"], ["Browser", C.blue, "globe"], ["Notes", C.milk, "notes"], ["Text Editor", C.milk, "editor"], ["Paint", C.milk, "paint"], ["Music", C.straw, "music"], ["Calendar", C.milk, "calendar"], ["Clock / Timer", C.matcha, "clock"], ["Preview", C.lav, "preview"], ["Settings", C.latte, "gear"], ["Store", C.straw, "store"], ["Spaces", C.milk, "spaces"]];
  apps.forEach(([n, bg, k], i) => { const x = 56 + (i % 7) * 134, y = 120 + Math.floor(i / 7) * 170; h += tile(x, y, 96, bg, k, dark) + TC(x - 20, y + 108, 136, n, `color:${txt(dark)}`) + TC(x - 20, y + 128, 136, bg === C.milk ? "milk" : bg === C.esp ? "espresso" : bg === C.honey ? "honey" : bg === C.straw ? "straw" : bg === C.blue ? "blue" : bg === C.matcha ? "matcha" : bg === C.lav ? "lavender" : "latte", `color:${sub(dark)}`); });
  h += T(40, 480, "Neighbours differ: no two dock tiles that sit next to each other share a colour.", `color:${sub(dark)}`) + T(40, 500, "Milk, espresso and latte tiles map through the dark table. Accent tiles do not change.", `color:${sub(dark)}`);
  // dock strip proof
  h += B(150, 560, 724, 80, `background:${dark ? "rgba(46,37,32,.9)" : "rgba(255,253,248,.7)"};border:1px solid ${dark ? "#55463C" : C.sand};border-radius:20px`) + apps.slice(0, 10).map((a, i) => tile(166 + i * 70, 576, 48, a[1], a[2], dark)).join("");
  return h;
}
/* 3: size ladder */
function sizes() {
  let h = stageBg(false) + T(40, 28, "Sizes to deliver", X2 + BD) + T(40, 70, "512 master, then 96, 48 (dock), 40 (launcher), 36 (drawer cards), 28 (taskbar), 20 and 16. Hand-tune 16 to 28.", `color:#6B5846`);
  const ladder = [96, 48, 40, 36, 28, 20, 16];
  [["Files", C.honey, "folder"], ["Calendar", C.milk, "calendar"], ["Activity Monitor", C.esp, "activity"], ["Preview", C.lav, "preview"]].forEach(([n, bg, k], r) => {
    const y = 120 + r * 150; let x = 150; h += T(40, y + 40, n, `color:#6B5846`);
    ladder.forEach((s) => { h += tile(x, y + (96 - s) / 2, s, bg, k) + TC(x - 20, y + 104, s + 40, String(s), `color:#6B5846`); x += s + 36; });
  });
  h += T(40, 740, "The 16 and 20 px versions keep only the silhouette and the accent; detail is dropped, not shrunk.", `color:#6B5846`);
  return h;
}
/* 4: wallpaper zones */
function zones() {
  let h = WP + menubar();
  const dashed = (x, y, w, hh, c, label) => B(x, y, w, hh, `border:2px dashed ${c};border-radius:10px;background:${c}22`) + T(x + 8, y + 6, label, `color:${C.esp};background:${C.milk}cc;border-radius:6px;padding:0 4px`);
  h += dashed(0, 0, 1024, 28, "#B5473B", "menu bar") + dashed(696, 36, 316, 720, "#7F93C9", "drawer / toast / control centre") + dashed(210, 692, 604, 64, "#9CBF9B", "dock") + dashed(48, 56, 640, 300, C.honey, "Today cards, launcher") + dashed(96, 380, 560, 200, C.straw, "Files and app windows");
  h += B(740, 80, 160, 160, "background:rgba(255,253,248,0)") + T(60, 610, "Calm zones: low detail, mid-light. Sun and cow go where glass rarely sits.", `color:${C.esp};background:${C.milk}cc;border-radius:6px;padding:2px 6px`) + T(60, 632, "Primary text on glass over any of these must stay at 7:1 or better:", `color:${C.esp};background:${C.milk}cc;border-radius:6px;padding:2px 6px`) + T(60, 654, "python3 docs/ui-concepts/art/check-wallpaper.py wallpaper.png [--dark]", `color:${C.esp};background:${C.milk}cc;border-radius:6px;padding:2px 6px`);
  return h;
}
/* 5: boot splash */
function splash() {
  const one = (dark, x0) => {
    const bg = dark ? "#382D27" : "#FBF6EC", fg = dark ? "#F4EBE0" : "#3A2E28", sb = dark ? "#BEA890" : "#6B5846";
    return B(x0, 60, 480, 360, `background:${bg};border-radius:14px;border:1px solid ${C.sand};overflow:hidden`,
      B(240 - 32, 74, 0, 0, `border-left:32px solid transparent;border-right:32px solid transparent;border-bottom:46px solid #3B6DB4`) + B(240 - 27, 84, 0, 0, `border-left:27px solid transparent;border-right:27px solid transparent;border-bottom:40px solid ${dark ? "#2E2520" : "#FFFDF8"}`) + circle(240 - 36, 104, 72, "#3B6DB4") + circle(240 - 31, 109, 62, dark ? "#2E2520" : "#FFFDF8") + B(214, 122, 8, 20, "background:#CFE0F6;border-radius:4px") + B(240 - 40, 184, 80, 8, `background:rgba(58,46,39,.12);border-radius:50%`) +
      TC(0, 214, 480, `<span style="color:${fg}">milk</span><span style="color:#3B6DB4">OS</span>`, X2) + B(240 - 55, 262, 110, 4, `background:${dark ? "#55463C" : "#E2D5BD"};border-radius:2px`, B(0, 0, 40, 4, `background:${C.straw};border-radius:2px`)) + TC(0, 278, 480, "booting", `color:${sb}`));
  };
  let h = stageBg(false) + T(40, 24, "Boot splash (shown at 480 wide here, 1024x768 in the build)", X2.replace("26.667px", "16px").replace("32px", "20px") + BD) + one(false, 24) + one(true, 520);
  h += T(40, 450, "Drop about 120 px tall with a small highlight and a faint shadow. Wordmark below, OS in blue.", `color:#6B5846`) + T(40, 472, "Progress bar 220x4, straw on sand. One status word in secondary text. No other animation.", `color:#6B5846`) + T(40, 494, "Group sits slightly above the middle. Fade to the desktop in about 216 ms (the current budget).", `color:#6B5846`);
  h += T(40, 540, "Night variant: dark cream background, dark milk drop fill, light wordmark.", `color:#6B5846`);
  return h;
}
const SLIDES = [
  { id: "feel", n: "1", name: "The feeling and the palette", tag: "Whole milk", draw: palette, cost: "easy", fam: "Where to start.", nw: "Soft, calm, warm, a little playful. Use only the SPEC tokens. Three tests for every asset: squint, neighbour, both themes.", helps: "Anyone drawing or choosing an asset.", why: "No engineering cost; it is the brief.", els: ["Tokens", "Three tests", "The never list"], font: "n/a" },
  { id: "icons", n: "2", name: "Icon family (light)", tag: "Flat glyph on a rounded tile", draw: () => family(false), cost: "easy", fam: "The current icon language, kept.", nw: "Tile radius 24%, glyph 55 to 60%, one accent, ink glyph on accent tiles. Colour map per app so neighbours differ. Placeholders only: the art lead draws the real ones.", helps: "Dock, launcher, drawer, Files and Welcome all use the same set.", why: "Assets embedded at the exact sizes the shell draws.", els: ["14 apps", "Colour map", "Dock strip proof"], font: "n/a" },
  { id: "icons-dark", n: "3", name: "Icon family (dark)", tag: "Chocolate milk", draw: () => family(true), cost: "easy", fam: "The same icons on the dark theme.", nw: "Milk, espresso and latte tiles map through the dark table; accent tiles stay the same with an ink glyph.", helps: "The dark theme.", why: "A -dark variant per size.", els: ["Dark table", "Accent tiles unchanged"], font: "n/a" },
  { id: "sizes", n: "4", name: "Sizes", tag: "512 down to 16", draw: sizes, cost: "easy", fam: "One master per app.", nw: "Deliver 96, 48, 40, 36, 28, 20 and 16. Hand-tune the small ones: drop detail, keep the silhouette and the accent.", helps: "Legibility in lists and the menu bar strip.", why: "Pre-rendered sizes avoid scaling at draw time.", els: ["7 sizes", "Hand-tuned small ones"], font: "n/a" },
  { id: "wall", n: "5", name: "Wallpaper zones", tag: "Where glass sits", draw: zones, cost: "easy", fam: "Wallpapers are art, but panels float over them.", nw: "Keep the top bar, the right band, the dock and the middle calm. Check each wallpaper with the contrast script; every region must pass 7:1.", helps: "Text over glass stays readable on any wallpaper.", why: "A script, run by the artist before hand-off.", els: ["Calm zones", "Contrast script", "Sun and cow placement"], font: "n/a" },
  { id: "splash", n: "6", name: "Boot splash", tag: "Light and night", draw: splash, cost: "easy", fam: "The current splash, refined.", nw: "Drop, wordmark with OS in blue, a 220x4 straw progress bar, one status word. A night variant in dark cream.", helps: "First impression in the first second.", why: "One image plus a progress bar drawn on top.", els: ["Drop", "Wordmark", "Progress bar", "Night variant"], font: "Wordmark in the pixel face at 3x." },
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
writeFileSync(join(here, "art-direction.html"), buildPage(SLIDES, {
  title: "milkOS: art direction", h1: "milkOS: art direction (reference sheet)",
  sub: "Visual reference for ART.md. The glyphs are placeholders so the proportions are clear. The art lead draws the real assets.",
  planTitle: "Hand-off",
  plan: "<p>Deliver masters and the exact sizes with a contact sheet (all icons at 48 and 16, light and dark). Wallpapers must pass <code>art/check-wallpaper.py</code>. Sounds follow the table in ART.md and must pass <code>wav_check.py</code>. The UI lead reviews the contact sheet before engineers embed anything.</p>",
  file: "art-direction.html" }));
console.log("wrote art-direction.html");
