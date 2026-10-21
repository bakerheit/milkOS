// Six alternatives to the bottom dock, drawn on the milkOS 1.0 desktop. Run: node docs/ui-concepts/dock-alternatives.mjs
// Writes dock-alternatives.html. Importing v1.mjs re-writes milkos-1.0.html and SPEC.md with identical content.
import { B, T, TC, TR, TRi, icon, drop, C, X2, X3, BD } from "./scenes.mjs";
import { TOK, WP, glass, shadow as shadowLayers, menubar, dock, lightsX, filesWin, shellWin, dim } from "./v1.mjs";
import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const shadow = (x, y, w, h, r, layers = TOK.shadow.window) => shadowLayers(x, y, w, h, r, layers);
const circle = (x, y, d, bg, extra = "") => B(x, y, d, d, `background:${bg};border-radius:50%;${extra}`);
const INK = "#3A2E27";
const APPS = [["folder", C.honey, "Files"], ["term", C.esp, "Terminal"], ["globe", C.blue, "Browser"], ["note", C.milk, "Notes"], ["photo", C.milk, "Paint"], ["music", C.straw, "Music"], ["cal", C.milk, "Calendar"], ["gear", C.latte, "Settings"]];
const iconOf = (a, x, y, s) => icon(x, y, s, a[0], a[1], a[0] === "term" ? C.cream : C.esp, a[1] === C.milk ? `border:1px solid ${C.sand}` : "");
const running = [0, 1, 3];
const milkDrop = (x, y, s) => B(x, y, s, s, `background:${C.milk};border:1px solid ${C.sand};border-radius:${Math.round(s * 0.28)}px`, circle(s * 0.28, s * 0.22, s * 0.44, C.milk, `border:${Math.max(2, s * 0.08)}px solid ${C.esp}`));
const tip = (x, y, t) => B(x, y, t.length * 8 + 20, 26, `background:rgba(255,253,248,.95);border:1px solid ${C.sand};border-radius:10px`) + T(x + 10, y + 5, t);
const desk = (menuApp = "Files", windows = true) => WP + menubar(menuApp) + (windows ? filesWin(110, 70, false) + shellWin(430, 270, true) : "");
const widthOf = (o) => o;

/* ---- 1. Left rail ---- */
function rail() {
  let h = WP + menubar() + filesWin(110, 70, false) + shellWin(440, 280, true);
  const x = 8, y = 36, w = 56, hh = 724;
  let inner = "";
  inner += milkDrop(8, 10, 40);
  inner += B(10, 60, 36, 1, `background:${C.sand}`);
  APPS.forEach((a, i) => { const iy = 74 + i * 52; inner += iconOf(a, 8, iy, 40) + (running.includes(i) ? B(1, iy + 17, 4, 6, `background:${C.cocoa};border-radius:2px`) : ""); });
  inner += B(10, hh - 108, 36, 1, `background:${C.sand}`) + iconOf(["folder", C.cream, "Downloads"], 8, hh - 98, 40).replace(C.cream, C.cream) + iconOf(["doc", C.cream, "Trash"], 8, hh - 50, 40);
  h += glass(x, y, w, hh, 20, inner);
  h += tip(72, 36 + 74 + 7, "Files   2 windows");
  return h;
}

/* ---- 2. Taskbar ---- */
function taskbar() {
  let h = WP + menubar() + filesWin(110, 60, true) + shellWin(430, 260, false);
  const y = 732, hh = 36;
  let inner = milkDrop(6, 4, 28) + B(40, 6, 1, 24, `background:${C.sand}`);
  [0, 1, 2, 5].forEach((ai, i) => { inner += iconOf(APPS[ai], 48 + i * 34, 4, 28); });
  inner += B(188, 6, 1, 24, `background:${C.sand}`);
  const wins = [["Documents", "folder", C.honey, true, 0], ["andrew: sh", "term", C.esp, false, 0], ["Notes + 2 tabs", "note", C.milk, false, 1]];
  wins.forEach((wn, i) => {
    const wx = 198 + i * 178;
    inner += B(wx, 3, 170, 30, `background:${wn[3] ? C.straw + "44" : "rgba(255,253,248,.6)"};border:${wn[3] ? 2 : 1}px solid ${wn[3] ? C.straw : C.sand};border-radius:9px`, iconOf([wn[1], wn[2], ""], 4, 3, 22) + T(32, 6, wn[0].length > 14 ? wn[0].slice(0, 13) + "~" : wn[0]) + (wn[4] ? B(150, 9, 12, 12, `background:${C.straw};border-radius:6px`) : ""));
  });
  inner += TRi(1024, 1012, 10, "Spaces  1  2  3", `color:${C.cocoa}`);
  h += B(0, y, 1024, hh, `background:rgba(255,253,248,.86);border-top:1px solid rgba(255,255,255,.9)`, "") + B(0, y, 1024, hh, "", inner);
  return h;
}

/* ---- 3. Apps in the menu bar ---- */
function menubarApps() {
  let h = WP + menubar() + filesWin(110, 70, true) + shellWin(430, 270, false);
  // strip of running apps in the menu bar, between the app menus and the status area
  const x0 = 520;
  h += B(x0 - 12, 3, 280, 22, `background:rgba(255,253,248,.55);border-radius:11px`);
  h += milkDrop(x0 - 6, 5, 18);
  [0, 1, 3, 5].forEach((ai, i) => { const ix = x0 + 26 + i * 32; h += (ai === 0 ? B(ix - 3, 5, 24, 18, `background:${C.straw}55;border-radius:6px`) : "") + iconOf(APPS[ai], ix, 6, 16) + (running.includes(ai) ? B(ix + 6, 24, 4, 2, `background:${C.cocoa};border-radius:1px`) : ""); });
  h += T(x0 + 160, 5, "+ 4 more", `color:${C.cocoa}`);
  h += tip(x0 + 22, 34, "Files   2 windows   Cmd-1");
  h += T(40, 700, "No dock, no taskbar: the whole bottom of the screen is windows.", `color:${C.esp}`);
  return h;
}

/* ---- 4. Auto-hide dock with window previews ---- */
function peek() {
  let h = WP + menubar() + filesWin(110, 60, false) + shellWin(440, 250, false);
  h += dock([0, 1, 3]);
  // preview card above the Files icon
  const d = TOK.dock, n = APPS.length, dw = d.pad * 2 + n * d.icon + (n - 1) * d.gap, dx = (1024 - dw) / 2, ix = dx + d.pad + d.icon / 2;
  const px = ix - 18, py = 430, pw = 330, ph = 236;
  let inner = T(16, 12, "<b>Files</b>   2 windows") + circle(pw - 30, 12, 16, "rgba(255,253,248,.0)");
  [["Documents", 0], ["Downloads", 1]].forEach((wnd, i) => {
    const wx = 14 + i * 156;
    inner += B(wx, 40, 148, 150, `background:${i === 0 ? C.straw + "33" : "rgba(255,253,248,.7)"};border:${i === 0 ? 2 : 1}px solid ${i === 0 ? C.straw : C.sand};border-radius:12px;overflow:hidden`,
      B(6, 6, 136, 112, `background:${C.milk};border:1px solid ${C.sand};border-radius:6px;overflow:hidden`, B(0, 0, 136, 12, `background:${C.cream}`) + B(0, 12, 34, 100, `background:${C.oat}`) + [0, 1, 2].map((k) => B(44 + k * 28, 26, 20, 20, `background:${C.honey}66;border-radius:5px`)).join("")) + TC(0, 124, 148, wnd[0]));
  });
  inner += T(16, 202, "Click to focus   Middle-click to close   Cmd-` cycles", `color:${C.cocoa}`);
  h += glass(Math.max(20, px - 140), py, pw, ph, 16, inner, { alpha: 0.88 });
  h += tip(dx + 10, 668, "hover for previews");
  h += B(442, 760, 140, 4, `background:${C.latte};border-radius:2px`);
  return h;
}

/* ---- 5. Dots pill ---- */
function dots() {
  let h = WP + menubar() + filesWin(110, 70, true) + shellWin(430, 270, false);
  const w = 232, x = (1024 - w) / 2, y = 722, hh = 34;
  let inner = milkDrop(5, 4, 26);
  [0, 1, 3].forEach((ai, i) => {
    const on = ai === 0, cx = 46 + i * 56;
    inner += B(cx, 5, on ? 52 : 40, 24, `background:${on ? C.straw + "55" : "rgba(255,253,248,.0)"};border-radius:12px`) + iconOf(APPS[ai], cx + 4, 6, 20) + (on ? T(cx + 28, 5, "2", `color:${C.cocoa}`) : "") ;
  });
  inner += B(w - 38, 6, 1, 22, `background:${C.sand}`) + TC(w - 34, 8, 28, "&#9776;", `color:${C.cocoa}`);
  h += glass(x, y, w, hh, 17, inner);
  h += T(x - 300, 728, "Ctrl-Space opens everything else", `color:${C.esp}`).replace(`left:${x - 300}px`, `left:${x - 270}px`);
  h += tip(x + 20, 690, "Files   2 windows");
  return h;
}

/* ---- 6. Corner fan ---- */
function fan() {
  let h = WP + menubar() + filesWin(110, 70, false) + shellWin(430, 270, false) + dim(0.22);
  const cx = 44, cy = 724;
  APPS.forEach((a, i) => {
    const ang = (-6 - i * (78 / 7)) * Math.PI / 180, r = 270, x = cx + r * Math.cos(ang), y = cy + r * Math.sin(ang);
    h += shadow(x - 25, y - 25, 50, 50, 14, TOK.shadow.float) + iconOf(a, x - 24, y - 24, 48) + (running.includes(i) ? circle(x + 14, y - 28, 12, C.straw, `border:2px solid ${C.milk}`) : "") + T(x + 28, y - 8, a[2], `color:${C.milk};background:rgba(58,46,40,.55);border-radius:6px;padding:0 4px`);
  });
  h += shadow(cx - 30, cy - 30, 60, 60, 18, TOK.shadow.float) + B(cx - 30, cy - 30, 60, 60, `background:${C.milk};border:2px solid ${C.straw};border-radius:18px`, circle(18, 14, 22, C.milk, `border:3px solid ${C.esp}`));
  h += T(120, 740, "Tap or press the milk drop. Flick toward an app.", `color:${C.milk}`);
  return h;
}
const SLIDES = [
  { id: "rail", n: "1", name: "Left rail", tag: "A slim vertical dock down the left edge", draw: rail, cost: "easy", used: "56px of 1024 wide (5%) instead of 76px of 768 tall (10%)", fam: "Familiar from GNOME, elementary and Linux desktops: icons in a column, a dot beside the running ones, a label on hover.", nw: "On a 4:3 screen vertical space is the tight one, so the rail trades a little width for 76 more pixels of window height. Skim's milk drop sits at the top, Downloads and Trash at the bottom.", helps: "Anyone who works in tall windows (the terminal, documents).", why: "The dock code with a rotated layout and a tooltip on the right. Easiest of the six, and it is just Settings > Dock > Position: Left.", els: ["Glass rail (56 wide)", "Skim button", "Pinned apps", "Running dots", "Downloads and Trash", "Hover label"], font: "Labels only appear on hover, so Unifont is no problem." },
  { id: "taskbar", n: "2", name: "Taskbar", tag: "A bottom bar with windows as labelled buttons", draw: taskbar, cost: "medium", used: "36px of 768 tall (5%)", fam: "The Windows layout everyone knows: launcher at the left, pinned apps, then one labelled button per window, tray at the right.", nw: "With Tabs everywhere, a tab group is one button with a small count badge instead of one button per window. Spaces are a mini switcher at the right edge.", helps: "People who juggle many windows and want to read their titles. It is the best answer to the question \"what is open?\".", why: "A row layout and a window list. The extra work is truncating titles and the tab-group badge.", els: ["Skim button", "Pinned apps", "Window buttons", "Tab-group badge", "Spaces switcher", "Focused window outline"], font: "Window titles are cut at about 13 characters in Unifont." },
  { id: "menubar", n: "3", name: "Apps in the menu bar", tag: "No dock at all", draw: menubarApps, cost: "easy", used: "0px extra", fam: "The menu bar stays as it is. The running apps sit in it as small icons, like the tray icons people already use.", nw: "The whole bottom of the screen is windows. Click an icon to focus, hover for the window list, Cmd-1 to Cmd-9 jump. The rest lives in Skim.", helps: "Small screens and people who barely use a mouse for launching.", why: "A strip of 16px icons in the bar. Needs an overflow when more than about six apps are open.", els: ["App strip in the menu bar", "Skim button", "Running dots", "Hover list", "Overflow (+ 4 more)"], font: "Icons only, so no font issue. Tooltips use Unifont." },
  { id: "peek", n: "4", name: "Auto-hide with previews", tag: "The dock appears at the edge, with window previews", draw: peek, cost: "medium", used: "4px when hidden", fam: "The macOS dock, set to hide, with a thin line where it lives.", nw: "Hover an app and a card shows a small preview of each of its windows to click. It answers \"which Files window?\" without opening Spaces.", helps: "Anyone who wants the dock's look with the whole screen free.", why: "Previews are scaled blits of each window's own buffer, which the compositor already has. The hover card is a glass panel.", els: ["Thin edge line", "Reveal on hover", "Per-app preview card", "Click to focus", "Middle-click to close"], font: "Previews are scaled pictures; only the card titles are text." },
  { id: "dots", n: "5", name: "Dots pill", tag: "A tiny pill with only what is running", draw: dots, cost: "easy", used: "34px of 768 tall, 232 wide", fam: "A floating pill like the dock, only smaller.", nw: "It shows the three or four apps you are using and a count of their windows. Everything else is one keystroke away in Skim, which is the point of the pill.", helps: "People who already open apps by typing, and anyone who finds a big dock noisy.", why: "A small glass panel with a short list. The smallest amount of UI of the six.", els: ["Glass pill", "Skim button", "Running apps with counts", "Overflow menu", "Hover label"], font: "Almost no text." },
  { id: "fan", n: "6", name: "Corner fan", tag: "One button in the corner that fans out the apps", draw: fan, cost: "medium", used: "60px square, nothing when closed", fam: "A floating launcher button, like on tablets and some Android skins.", nw: "Press the milk drop and the apps fan out along an arc with labels and a ring on the running ones. Flick toward an app. It works with a thumb, a mouse or a game controller.", helps: "Touch and controller use, and a clean screen the rest of the time.", why: "Places icons on a quarter circle and picks by angle. A little animation helps but is not needed.", els: ["Corner button", "Arc of apps", "Labels", "Running badges", "Flick to choose"], font: "Labels are drawn on dark chips so they read on any wallpaper." },
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
writeFileSync(join(here, "dock-alternatives.html"), buildPage(SLIDES, {
  title: "milkOS: alternatives to the dock", h1: "milkOS: alternatives to the dock",
  sub: "Six ways to launch and switch apps, each keeping something people already know. Each notes how much screen it costs: on a 1024 by 768 screen, height is the scarce thing.",
  planTitle: "Design notes",
  plan: `<p><b>Space used:</b> today's dock takes 76px of the 768px height (10%). Left rail 64px of 1024 width (5%). Taskbar 36px (5%). Apps in the menu bar 0px. Auto-hide 4px. Dots pill 34px. Corner fan 0px when closed.</p><p style="margin-top:8px"><b>My suggestion:</b> keep the dock as the default and add two switches in Settings > Dock: <b>Position</b> (Bottom or Left, the rail) and <b>Auto-hide</b> (with window previews, since the compositor already holds each window's buffer). The taskbar is the right extra mode for people coming from other desktops, the dots pill and the menu-bar strip suit minimal setups, and the corner fan is the one to build only if touch or a controller becomes a target.</p>`,
  file: "dock-alternatives.html" }));
console.log("wrote dock-alternatives.html");
