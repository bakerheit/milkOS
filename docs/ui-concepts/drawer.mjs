// Key states of the chosen notification drawer (side drawer). Run: node docs/ui-concepts/drawer.mjs
// Writes drawer.html. Importing notifications.mjs / v1.mjs rewrites their outputs with identical content.
import { B, T, TC, TR, TRi, icon, drop, C, X2, BD } from "./scenes.mjs";
import { TOK, WP, glass, shadow as shadowLayers, menubar, dock, lightsX } from "./v1.mjs";
import { NOTE, card, side, tabsWindow, dockBadged, badge, pill, circle } from "./notifications.mjs";
import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
const D = TOK.drawer;
const bell = (x, y, n, moon = false) => B(x, y + 2, 14, 12, `background:${C.esp};border-radius:7px 7px 2px 2px`) + B(x + 5, y + 14, 4, 3, `background:${C.esp};border-radius:2px`) + (n ? badge(x + 8, y - 6, n) : "") + (moon ? circle(x + 10, y - 4, 14, C.lav, `border:2px solid ${C.milk}`) : "");
const desktop = (n, moon = false) => WP + menubar() + B(828, 4, 24, 22, "background:rgba(255,253,248,.0)") + bell(836, 5, n, moon) + tabsWindow();
const switchUi = (x, y, on) => B(x, y, 44, 24, `background:${on ? C.straw : C.sand};border-radius:12px`, circle(on ? 22 : 2, 2, 20, C.milk));

/* 1: toast arrives */
function arrive() {
  let h = desktop(3);
  const t = TOK.toast, x = 1024 - t.right - t.w;
  h += glass(x, t.top, t.w, t.h, TOK.radius.toast, icon(12, 12, 40, "photo", C.straw, C.esp) + T(64, 12, "<b>Mum</b>") + TRi(t.w, t.w - 12, 12, "now", `color:${C.cocoa}`) + T(64, 32, "Photos from the weekend", `color:${C.cocoa}`));
  h += B(x - 220, t.top + 8, 204, 48, "background:rgba(255,253,248,.0)") + T(x - 232, t.top + 22, "shows for 5 s, pauses on hover", `color:${C.esp}`).replace("white-space:pre", "white-space:pre;text-align:right");
  return h + dockBadged();
}
/* 3: group expanded, selected card, key hints */
function expanded() {
  let h = desktop(3) + dockBadged();
  const x = 1024 - D.right - D.w, y = D.top, w = D.w, hh = 768 - D.top - D.bottom;
  let inner = T(18, 14, "<b>Notifications</b>") + TRi(w, w - 18, 14, "Clear all", `color:${C.cocoa}`) + pill(14, 46, 64, "All", true, D.chipH) + pill(84, 46, 96, "Mentions", false, D.chipH) + pill(186, 46, 80, "Today", false, D.chipH);
  inner += T(18, 88, "&#9662; MESSAGES", `color:${C.cocoa}`) + TRi(w, w - 18, 88, "4", `color:${C.cocoa}`);
  const more = [NOTE.mum, NOTE.sam, { ic: "photo", tone: C.straw, title: "Mum", body: "Are you coming Sunday?", t: "9:40" }, { ic: "note", tone: C.blue, title: "Sam", body: "PR is up for review", t: "9:12" }];
  more.forEach((n, i) => { inner += card(12, 110 + i * 72, w - 24, n, { sel: i === 1 }); });
  inner += B(w - 38, 190, 24, 24, `background:${C.milk};border:1px solid ${C.sand};border-radius:12px`) + TC(w - 38, 195, 24, "x", `color:${C.cocoa}`);
  inner += T(18, 410, "&#9656; CALENDAR", `color:${C.cocoa}`) + TRi(w, w - 18, 410, "1", `color:${C.cocoa}`) + T(18, 440, "&#9656; SYSTEM", `color:${C.cocoa}`) + TRi(w, w - 18, 440, "2", `color:${C.cocoa}`);
  inner += B(12, hh - 134, w - 24, 112, `background:rgba(255,253,248,.82);border:1px solid rgba(255,255,255,.9);border-radius:14px`, T(14, 12, "KEYS", `color:${C.cocoa}`) + T(14, 34, "Up / Down   select") + T(14, 54, "Return      open the source") + T(14, 74, "Backspace   dismiss") + T(14, 94, "Left / Right  collapse group", `color:${C.cocoa}`));
  h += glass(x, y, w, hh, TOK.radius.launcher, inner, { alpha: D.alpha });
  return h;
}
/* 4: all caught up, Focus on */
function empty() {
  let h = desktop(0, true) + dock([0, 1]);
  const x = 1024 - D.right - D.w, y = D.top, w = D.w, hh = 768 - D.top - D.bottom;
  let inner = T(18, 14, "<b>Notifications</b>") + pill(14, 46, 64, "All", true, D.chipH) + pill(84, 46, 96, "Mentions", false, D.chipH) + pill(186, 46, 80, "Today", false, D.chipH);
  inner += circle(w / 2 - 36, 150, 72, C.matcha + "66", `border:3px solid ${C.matcha}`) + TC(w / 2 - 36, 168, 72, "&#10003;", "font-size:26.667px;line-height:32px;color:" + C.esp) + TC(0, 244, w, "<b>All caught up</b>") + TC(0, 266, w, "Nothing needs you right now.", `color:${C.cocoa}`);
  inner += B(12, 320, w - 24, 110, `background:rgba(255,253,248,.82);border:1px solid rgba(255,255,255,.9);border-radius:14px`, T(14, 10, "TODAY", `color:${C.cocoa}`) + T(14, 32, "<b>2:30 PM</b>  Dentist") + T(14, 52, "5:00 PM  Pick up bike", `color:${C.cocoa}`) + T(14, 72, "7:30 PM  Dinner", `color:${C.cocoa}`));
  inner += B(12, hh - 62, w - 24, D.dndH, `background:${C.lav}44;border:1px solid ${C.lav};border-radius:14px`, T(14, 14, "Focus  on until 12:00") + switchUi(w - 24 - 58, 10, true));
  h += glass(x, y, w, hh, TOK.radius.launcher, inner, { alpha: D.alpha });
  h += T(40, 590, "Moon on the bell: toasts and sounds are held,", `color:${C.esp}`) + T(40, 608, "but cards still collect in the drawer.", `color:${C.esp}`);
  return h;
}
const SLIDES = [
  { id: "arrive", n: "1", name: "A notification arrives", tag: "Toast, then it settles in the drawer", draw: arrive, cost: "easy", fam: "A message lands while you work.", nw: "A toast (264 by 64) slides in top right and stays 5 seconds, pausing while the pointer is on it. The menu bar bell and the app's dock icon pick up a count.", helps: "Clicking the toast opens the drawer on that item. Ignore it and it is waiting in the drawer.", why: "Toast and badge only. Reuses the toast and glass panels already in the spec.", els: ["Toast (264x64)", "Bell count", "Dock badge", "5 second timer", "Pause on hover"], font: "Keep the toast title to about 10 characters and the body to about 22." },
  { id: "open", n: "2", name: "Drawer open", tag: "The chosen look", draw: side, cost: "easy", fam: "Click the bell or press Ctrl-N.", nw: "Grouped cards, filter chips, today's events and Focus. Esc or a click outside closes it.", helps: "Selected card has the straw outline.", why: "One glass panel and a list of cards.", els: ["Header and chips", "Groups", "Cards", "Today card", "Focus"], font: "Bodies are cut with a tilde around 30 characters." },
  { id: "expanded", n: "3", name: "Working in the drawer", tag: "Groups, keys and dismissing", draw: expanded, cost: "easy", fam: "Arrow keys move through the list.", nw: "A group is expanded to show all four messages, the selected card shows a small x on hover, and the keys are listed (Return opens the source, Backspace dismisses, Left and Right collapse a group).", helps: "Everything works from the keyboard first, with the pointer as a bonus.", why: "A selection index, a collapsed flag per group and a delete.", els: ["Expandable groups", "Selection outline", "Hover x", "Key map"], font: "Group arrows are plain characters, no icons needed." },
  { id: "empty", n: "4", name: "All caught up", tag: "Empty state with Focus on", draw: empty, cost: "easy", fam: "Nothing is waiting.", nw: "A calm check mark, today's events, and Focus switched on until 12:00 (lavender row, moon on the bell). Toasts and sounds are held, but cards still collect.", helps: "A quiet drawer feels like a reward, not an empty box.", why: "One empty-state layout and a timer for Focus.", els: ["Empty state", "Today card", "DND row", "Moon on the bell"], font: "The check is a plain character at 2x." },
];
/* ------------------------------ page ------------------------------ */
const N = SLIDES.length;
const id = (i) => `n${((i % N) + N) % N}`;
const svg = (s) => `<svg class="scene" viewBox="0 0 1024 768" role="img" aria-label="Mockup of ${s.name}"><foreignObject x="0" y="0" width="1024" height="768"><div class="stage">${s.draw()}</div></foreignObject></svg>`;
const rules = SLIDES.map((s, i) => `#r-${id(i)}:checked ~ .wrap .p-${id(i)}{display:grid}\n#r-${id(i)}:checked ~ .wrap .n-${id(i)}{display:flex}\n#r-${id(i)}:checked ~ .wrap .t-${id(i)}{border-color:var(--accent)}`).join("\n");

const html = `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>milkOS: notification drawer states</title>
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
<header><div><h1>milkOS: the notification drawer, key states</h1><div class="sub">The side drawer you picked, shown through a notification's life: arriving, opening the drawer, working through it, and all caught up. Numbers are in SPEC.md. Use Prev / Next or tap a thumbnail.</div></div>
${SLIDES.map((s, i) => `<div class="nav n-${id(i)}"><label class="btn" for="r-${id(i - 1)}" role="button" tabindex="0" aria-label="Previous">&larr; Prev</label><span class="count">${i + 1} / ${N}</span><label class="btn" for="r-${id(i + 1)}" role="button" tabindex="0" aria-label="Next">Next &rarr;</label></div>`).join("")}
</header>
${SLIDES.map((s, i) => `<article class="panel p-${id(i)}"><div class="stagebox"><div class="stagewrap">${svg(s)}</div><div class="cap">${s.n} ${s.name}: drawn 1:1 at 1024×768 and scaled to fit.</div></div>
<aside class="notes"><h2>${s.n}. ${s.name}</h2><p class="tag">${s.tag}</p><span class="cost ${s.cost}">${s.cost}</span>
<h3>When</h3><p>${s.fam}</p><h3>What you see</h3><p>${s.nw}</p><h3>Detail</h3><p>${s.helps}</p><h3>Build note</h3><p>${s.why}</p>
<h3>Main elements</h3><ul class="chips">${s.els.map((e) => `<li>${e}</li>`).join("")}</ul><h3>Font note</h3><p>${s.font}</p></aside></article>`).join("")}
<div class="strip">${SLIDES.map((s, i) => `<label class="thumb t-${id(i)}" for="r-${id(i)}" tabindex="0" role="button" aria-label="Show ${s.name}"><div class="tw">${svg(s)}</div><div class="tl"><b>${s.n}. ${s.name}</b><span>${s.cost}</span></div></label>`).join("")}</div>
<section class="plan"><h2>Next</h2><p>Triage actions (Done, Later, Mute) and notifications attached to window tabs can be added on top of this later without changing it. For now this is the whole drawer.</p></section>
</div>
<script>addEventListener('keydown',function(e){var r=[].slice.call(document.querySelectorAll('.r')),c=r.findIndex(function(x){return x.checked});if(e.key==='ArrowRight')r[(c+1)%r.length].checked=true;else if(e.key==='ArrowLeft')r[(c-1+r.length)%r.length].checked=true;else if((e.key==='Enter'||e.key===' ')&&e.target.tagName==='LABEL'){e.preventDefault();e.target.click()}})</script>
</body></html>`;

const here = dirname(fileURLToPath(import.meta.url));
writeFileSync(join(here, "drawer.html"), html);
console.log("wrote drawer.html", html.length, "bytes");
