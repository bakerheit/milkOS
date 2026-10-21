// Five 'familiar plus one new thing' ideas built on the milkOS 1.0 desktop. Run: node docs/ui-concepts/familiar-plus.mjs
// Writes familiar-plus.html. Importing v1.mjs re-writes milkos-1.0.html and SPEC.md with identical content.
import { B, T, TC, TR, TRi, icon, drop, C, X2, BD } from "./scenes.mjs";
import { TOK, WP, glass, shadow as shadowLayers, menubar, dock, win, lightsX, filesWin, shellWin, dim, toast } from "./v1.mjs";
import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const ESP = "rgba(58,46,40,";
const shadow = (x, y, w, h, r, layers = TOK.shadow.window) => shadowLayers(x, y, w, h, r, layers);
const circle = (x, y, d, bg, extra = "") => B(x, y, d, d, `background:${bg};border-radius:50%;${extra}`);
const newBadge = (x, y) => B(x, y, 44, 18, `background:${C.straw};border-radius:9px`) + TC(x, y + 1, 44, "<b>NEW</b>", `color:${C.milk};font-size:12px;line-height:16px`);
const pill = (x, y, w, txt, on = false, h = 28) => B(x, y, w, h, `background:${on ? C.esp : "rgba(255,253,248,.85)"};border:1px solid ${on ? C.esp : C.sand};border-radius:${h / 2}px`) + TC(x, y + (h - 16) / 2, w, txt, `color:${on ? C.cream : C.esp}`);
const base = () => WP + menubar();

/* ---------- A. Tabs everywhere ---------- */
function tabs() {
  let h = base();
  const x = 90, y = 70, w = 660, hh = 430;
  // one window whose title bar is a tab strip
  h += shadow(x, y, w, hh, 12) + B(x, y, w, hh, `background:${C.milk};border-radius:12px;border:1px solid ${C.sand};overflow:hidden`,
    B(0, 0, w, 38, `background:${C.cream};border-bottom:1px solid ${C.sand}`) + lightsX(13, 12) +
    [["Documents", 1], ["andrew: sh", 0], ["notes.md", 0]].map((t, i) => { const tx = 84 + i * 150; return B(tx, 6, 144, 32, `background:${t[1] ? C.milk : "transparent"};border:${t[1] ? 1 : 0}px solid ${C.sand};border-bottom:0;border-radius:10px 10px 0 0`) + T(tx + 12, 11, t[1] ? `<b>${t[0]}</b>` : t[0], t[1] ? "" : `color:${C.cocoa}`) + TRi(w, tx + 134, 11, "x", `color:${C.cocoa}`); }).join("") +
    B(84 + 3 * 150 + 4, 10, 28, 24, `background:${C.oat};border-radius:8px`) + TC(84 + 3 * 150 + 4, 14, 28, "+") +
    B(0, 38, 160, hh - 40, `background:${C.oat};border-right:1px solid ${C.sand}`) + T(16, 52, "FAVOURITES", `color:${C.cocoa}`) + B(8, 76, 144, 24, `background:${C.straw}55;border-radius:8px`) + T(18, 80, "Documents") + T(18, 108, "Downloads") + T(18, 136, "Desktop") + T(18, 164, "Pictures") +
    [["folder", "projects"], ["folder", "school"], ["doc", "todo.txt"], ["photo", "bike.png"], ["note", "ideas.md"], ["folder", "kernel"]].map((f, i) => {
      const cx = 184 + (i % 4) * 116, cy = 62 + Math.floor(i / 4) * 116, isF = f[0] === "folder";
      return icon(cx + 20, cy, 56, f[0], isF ? C.honey + "55" : C.cream, isF ? C.cocoa : C.esp, isF ? "" : `border:1px solid ${C.sand}`) + TC(cx - 6, cy + 64, 108, f[1]);
    }).join(""));
  // drop highlight on the tab strip
  h += B(x + 84 + 3 * 150 - 4, y + 4, 184, 36, `background:${C.straw}33;border:2px dashed ${C.straw};border-radius:10px`) + T(x + 84 + 3 * 150 + 6, y + 12, "Drop to add as a tab", `color:${C.esp};${BD}`);
  // another window being dragged (ghost)
  h += shadow(560, 330, 330, 250, 12) + B(560, 330, 330, 250, `background:${C.milk};border-radius:12px;border:2px solid ${C.straw};opacity:.92;overflow:hidden`,
    B(0, 0, 330, 32, `background:${C.cream};border-bottom:1px solid ${C.sand}`) + lightsX(13, 10) + TC(0, 8, 330, "<b>todo.txt</b>") + T(18, 52, "[x] draft the UI spec") + T(18, 72, "[x] pick a look") + T(18, 92, "[ ] fix the allocator") + T(18, 112, "[ ] boot logo", `color:${C.cocoa}`));
  h += circle(716, 322, 20, C.esp, `border:3px solid ${C.milk}`);
  h += newBadge(x, y - 26) + T(x + 52, y - 24, "Merge windows into tabs", `color:${C.esp}`);
  return h + dock([0, 1]);
}

/* ---------- B. Projects ---------- */
function projects() {
  let h = WP;
  h += shadow(96, 70, 600, 400, 12);
  h += filesWin(96, 90, false) + shellWin(440, 300, true) + dim(0.18);
  h += menubar().replace("<b>Files</b>", "<b>Files</b>");
  // project pill in the menu bar
  h += B(300, 3, 170, 22, `background:${C.straw};border-radius:11px`) + TC(300, 5, 170, "<b>milkOS 1.0  &#9662;</b>", `color:${C.milk}`) + newBadge(476, 5);
  // switcher popover
  const px = 270, py = 38, pw = 440, ph = 392;
  const proj = [["milkOS 1.0", "4 windows   last used now", 1, [[8, 8, 56, 36, C.blue], [34, 24, 52, 28, C.esp]]], ["School", "2 windows   yesterday", 0, [[10, 10, 50, 34, C.honey]]], ["Bike repair", "1 window   Mon", 0, [[12, 12, 60, 30, C.matcha]]], ["Personal", "no windows   last week", 0, []]];
  let inner = T(18, 14, "PROJECTS", `color:${C.cocoa}`) + TRi(pw, pw - 18, 14, "Ctrl-` to switch", `color:${C.cocoa}`);
  proj.forEach((p, i) => {
    const ry = 42 + i * 74;
    inner += (p[2] ? B(8, ry - 2, pw - 16, 68, `background:${C.straw}33;border:1px solid ${C.straw};border-radius:12px`) : "") +
      B(18, ry + 6, 96, 52, `background:${C.cream};border:1px solid ${C.sand};border-radius:8px`, p[3].map((r) => B(r[0], r[1], r[2], r[3], `background:${r[4]}88;border-radius:4px`)).join("")) +
      T(128, ry + 10, `<b>${p[0]}</b>`) + T(128, ry + 32, p[1], `color:${C.cocoa}`) + (p[2] ? TRi(pw, pw - 24, ry + 20, "current", `color:${C.straw}`) : "");
  });
  inner += B(8, 342, pw - 16, 1, `background:${C.sand}`) + T(18, 356, "+ New project...", `color:${C.cocoa}`) + TRi(pw, pw - 18, 356, "Layout, files, notes saved", `color:${C.cocoa}`);
  h += glass(px, py, pw, ph, 18, inner, { alpha: 0.78 });
  return h + dock([0, 1]);
}

/* ---------- C. Time slider in every window ---------- */
function timeslider() {
  let h = base();
  const x = 96, y = 70, w = 700, hh = 470;
  h += shadow(x, y, w, hh, 12) + B(x, y, w, hh, `background:${C.milk};border-radius:12px;border:2px solid ${C.honey};overflow:hidden`,
    B(0, 0, w, 32, `background:#F4E6C4;border-bottom:1px solid ${C.sand}`) + lightsX(13, 10) + TC(0, 8, w, "<b>Documents</b>   (as of Mon 4:05 PM)") +
    circle(w - 40, 6, 22, C.honey) + B(w - 31, 10, 2, 8, `background:${C.esp}`) + B(w - 31, 17, 7, 2, `background:${C.esp}`) +
    B(0, 32, 160, hh - 34, `background:#F1E8D2;border-right:1px solid ${C.sand}`) + T(16, 46, "FAVOURITES", `color:${C.cocoa}`) + B(8, 70, 144, 24, `background:${C.straw}44;border-radius:8px`) + T(18, 74, "Documents") + T(18, 102, "Downloads") + T(18, 130, "Desktop") +
    [["folder", "projects", 0], ["doc", "todo.txt", 0], ["doc", "old-plan.txt", 1], ["photo", "bike.png", 0], ["note", "ideas.md", 0], ["folder", "kernel", 0]].map((f, i) => {
      const cx = 184 + (i % 4) * 120, cy = 54 + Math.floor(i / 4) * 116, isF = f[0] === "folder";
      return (f[2] ? B(cx - 6, cy - 6, 108, 104, `background:${C.honey}33;border:1px dashed ${C.honey};border-radius:12px`) : "") + icon(cx + 20, cy, 56, f[0], isF ? C.honey + "55" : C.cream, isF ? C.cocoa : C.esp, isF ? "" : `border:1px solid ${C.sand}`) + TC(cx - 6, cy + 64, 108, f[1]) + (f[2] ? TC(cx - 6, cy + 82, 108, "deleted since", `color:${C.straw}`) : "");
    }).join(""));
  h += B(x, y, w, hh, `background:rgba(201,170,110,.12);border-radius:12px`);
  // scrubber bar inside the window
  h += B(x + 1, y + hh - 118, w - 2, 117, `background:rgba(255,253,248,.96);border-top:1px solid ${C.sand}`) +
    T(x + 18, y + hh - 106, "Showing this folder as it was on Mon 4:05 PM", `color:${C.cocoa}`) +
    B(x + 18, y + hh - 70, w - 36, 8, `background:${C.sand};border-radius:4px`, B(0, 0, 330, 8, `background:${C.honey};border-radius:4px`)) +
    [60, 150, 230, 330, 420, 520, 600].map((tx, i) => B(x + 18 + tx, y + hh - 80, 2, 10, `background:${i === 3 ? C.honey : C.latte}`)).join("") +
    circle(x + 18 + 322, y + hh - 76, 20, C.honey, `border:3px solid ${C.milk}`) +
    T(x + 18, y + hh - 52, "Mon", `color:${C.cocoa}`) + T(x + 18 + 240, y + hh - 52, "Tue 10:41 (now)", `color:${C.cocoa}`) +
    B(x + w - 330, y + hh - 40, 98, 28, `background:${C.esp};border-radius:14px`) + TC(x + w - 330, y + hh - 34, 98, "Restore", `color:${C.cream}`) + pill(x + w - 224, y + hh - 40, 110, "Copy out") + pill(x + w - 106, y + hh - 40, 92, "Back to now");
  h += newBadge(x + w - 96, y - 24);
  return h + dock([0]);
}

/* ---------- D. Shelf + clipboard history ---------- */
function shelf() {
  let h = base() + filesWin(60, 70, true);
  // dragging a file toward the shelf
  h += icon(560, 190, 56, "photo", C.cream, C.esp, `border:1px solid ${C.sand};box-shadow:0 10px 24px rgba(58,46,40,.3)`) + TC(540, 252, 96, "bike.png", `background:${C.esp};color:${C.cream};border-radius:6px`);
  // the shelf on the right edge
  const sx = 880, sy = 60, sw = 132, sh = 540;
  let inner = T(14, 14, "SHELF", `color:${C.cocoa}`) + newBadge(sw - 54, 12);
  inner += icon(30, 52, 72, "photo", C.cream, C.esp, `border:1px solid ${C.sand}`) + TC(6, 128, 120, "trail.png");
  inner += B(10, 160, 112, 80, `background:${C.cream};border:1px solid ${C.sand};border-radius:10px`, T(8, 8, "fn kmain() {", `color:${C.cocoa}`) + T(8, 26, "  uart::init", `color:${C.cocoa}`) + T(8, 48, "snippet", `color:${C.latte}`));
  inner += icon(30, 256, 72, "doc", C.cream, C.esp, `border:1px solid ${C.sand}`) + TC(6, 332, 120, "notes.txt");
  inner += B(10, 368, 112, 140, `background:${C.straw}22;border:2px dashed ${C.straw};border-radius:14px`) + TC(10, 410, 112, "Drop to", `color:${C.straw}`) + TC(10, 430, 112, "hold here", `color:${C.straw}`);
  h += glass(sx, sy, sw, sh, 18, inner);
  // clipboard strip
  const cx = 150, cy = 600, cw = 700, ch = 80;
  let ci = T(16, 8, "CLIPBOARD   hold Ctrl-V, pick with the arrows", `color:${C.cocoa}`) + newBadge(cw - 60, 8);
  [["fn kmain() {", 1], ["https://milk.example", 0], ["Dear Sam,", 0], ["10:41", 0], ["image 800x600", 0], ["#FBF6EC", 0]].forEach((c, i) => {
    ci += B(14 + i * 112, 34, 106, 36, `background:${c[1] ? C.straw + "44" : "rgba(255,253,248,.9)"};border:${c[1] ? 2 : 1}px solid ${c[1] ? C.straw : C.sand};border-radius:10px;overflow:hidden`, T(8, 10, c[0].slice(0, 12)));
  });
  h += glass(cx, cy, cw, ch, 16, ci, { alpha: 0.78 });
  return h + dock([0]);
}

/* ---------- E. Live capsule in the menu bar ---------- */
function capsule() {
  let h = WP + filesWin(96, 90, false) + shellWin(440, 300, true) + menubar();
  // capsule(s) in the menu bar
  h += B(380, 2, 264, 24, `background:${C.esp};border-radius:12px`) + circle(392, 9, 10, C.matcha) + T(410, 4, "<b>Build passed</b>", `color:${C.cream}`) + T(520, 4, "2m   25:00", `color:${C.latte}`) + newBadge(650, 5);
  // expanded panel
  const px = 330, py = 36, pw = 364, ph = 322;
  let inner = "";
  const row = (y, ic, title, sub, right) => B(12, y, pw - 24, 62, `background:rgba(255,253,248,.7);border:1px solid ${C.sand};border-radius:14px`, icon(10, 10, 42, ic, C.cream, C.esp, `border:1px solid ${C.sand}`) + T(64, 10, `<b>${title}</b>`) + T(64, 30, sub, `color:${C.cocoa}`) + right);
  inner += row(12, "term", "cargo build", "passed 2m ago", B(pw - 120, 16, 76, 28, `background:${C.esp};border-radius:14px`) + TC(pw - 120, 22, 76, "Re-run", `color:${C.cream}`));
  inner += row(84, "clock", "Focus timer", "25:00 left: UI spec", B(pw - 120, 16, 76, 28, "background:rgba(255,253,248,.9);border:1px solid " + C.sand + ";border-radius:14px") + TC(pw - 120, 22, 76, "Pause"));
  inner += row(156, "folder", "Downloading ubuntu.iso", "1.9 of 3.1 GB, 4 min", B(64, 46, 200, 8, `background:${C.sand};border-radius:4px`, B(0, 0, 124, 8, `background:${C.blue};border-radius:4px`)));
  inner += row(228, "music", "Cream Soda Skies", "The Lactose Intolerants", T(pw - 112, 16, "&#9198;   &#9199;   &#9197;", `color:${C.esp}`));
  inner += T(16, ph - 22, "Anything that runs in the background lives here.", `color:${C.cocoa}`).replace(`top:${ph - 22}px`, `top:${ph - 30}px`);
  h += glass(px, py, pw, ph + 6, 18, inner, { alpha: 0.8 });
  return h + dock([0, 1]);
}

const SLIDES = [
  { id: "tabs", n: "A", name: "Tabs everywhere", tag: "Any window can become a tab in any other", draw: tabs, cost: "medium", fam: "Normal windows with the usual title bar and traffic lights.", nw: "Drag a window onto another one's title bar and it becomes a tab. Files, a terminal and your notes can live in one window. Drag a tab out to split it back.",
    helps: "Anyone with too many windows. It is how people already use browsers, applied to the whole OS.",
    why: "Needs a small window tree (a window holds a list of children instead of one surface) and drag-and-drop of title bars. The tab strip is plain rounded rects. No new rendering power needed.",
    els: ["Tab strip in the title bar", "Drop highlight", "Drag a window or a tab", "Merge and split", "Per-tab close", "Keyboard: Ctrl-1..9"],
    font: "Tab titles need to be short at 8 px per letter, so truncate with an ellipsis around 14 characters." },
  { id: "projects", n: "B", name: "Projects", tag: "Named workspaces that remember everything", draw: projects, cost: "medium", fam: "Looks like virtual desktops: a switcher, a shortcut, a menu bar item.", nw: "Each project is a named workspace that remembers which windows were open, where they sat, and which files and notes belong to it. Switch from milkOS to School and your whole setup follows, exactly as you left it.",
    helps: "People who juggle several things (school, work, hobbies) and lose their place every time they switch.",
    why: "Saving a layout is writing a list of windows (app, file, position, size) to a file, and restoring is reading it back. The switcher is a glass panel with small rectangles for thumbnails, no screenshots needed.",
    els: ["Project pill in the menu bar", "Project switcher panel", "Layout thumbnails", "Save and restore layouts", "Per-project files and notes", "Ctrl-` shortcut"],
    font: "Unifont is fine. The straw pill in the menu bar tells you where you are." },
  { id: "time", n: "C", name: "Time slider", tag: "Every window can look at its own past", draw: timeslider, cost: "hard", fam: "A normal Files window with one extra clock button in its title bar.", nw: "Click the clock and the window shows its contents as of an earlier time. Deleted files appear again (marked), and you can restore one or copy it out. Nothing is lost by accident.",
    helps: "Everyone who has ever deleted the wrong file. It is Time Machine, but inside the window you are already looking at.",
    why: "The hard part is underneath: the filesystem has to keep earlier versions (copy-on-write or snapshots). The UI is a slider, a tint over the window and three buttons. Ship it first for Files only, other apps later.",
    els: ["Clock button in the title bar", "Past-state tint", "Scrubber with ticks", "Deleted-file markers", "Restore / Copy out / Back to now", "Snapshot store"],
    font: "Unifont works. The honey tint and the slider carry the idea." },
  { id: "shelf", n: "D", name: "Shelf and clipboard", tag: "A place to hold things while you move around", draw: shelf, cost: "easy", fam: "Drag and drop, copy and paste. Nothing you do changes.", nw: "A shelf on the screen edge holds files and snippets while you go and find where they belong, and holding Ctrl-V shows your last few copies to pick from. Both keep things you would normally lose.",
    helps: "Anyone who has copied something, then copied something else, and lost the first thing.",
    why: "The shelf is a list of references and a drop target. The clipboard history is a ring of the last N items. The glass panels reuse the pre-blurred wallpaper. This is one of the cheapest ideas here.",
    els: ["Edge shelf (drop target)", "Held items", "Clipboard history strip", "Pick with arrows", "Clear / pin an item"],
    font: "Snippets are cropped at about 12 characters. A real font would let you show more." },
  { id: "capsule", n: "E", name: "Live capsule", tag: "Background things live in the menu bar", draw: capsule, cost: "easy", fam: "A normal menu bar and a normal desktop.", nw: "A small capsule in the middle of the menu bar shows what is running right now: a build, a timer, a download, music. Click it to open controls without switching windows, and it disappears when nothing is happening.",
    helps: "Everyone who waits for something while doing something else, which is everyone.",
    why: "It is one pill and one panel, driven by a short list of background jobs that each report a title, a status and an action. Very little drawing, and it uses glass you already have.",
    els: ["Capsule in the menu bar", "Expanding panel", "Job list (build, timer, download, music)", "Quick actions", "Auto-hide when idle"],
    font: "Unifont works. Keep the capsule text to about 12 characters." },
];

/* ------------------------------ page ------------------------------ */
const N = SLIDES.length;
const id = (i) => `n${((i % N) + N) % N}`;
const svg = (s) => `<svg class="scene" viewBox="0 0 1024 768" role="img" aria-label="Mockup of ${s.name}"><foreignObject x="0" y="0" width="1024" height="768"><div class="stage">${s.draw()}</div></foreignObject></svg>`;
const rules = SLIDES.map((s, i) => `#r-${id(i)}:checked ~ .wrap .p-${id(i)}{display:grid}\n#r-${id(i)}:checked ~ .wrap .n-${id(i)}{display:flex}\n#r-${id(i)}:checked ~ .wrap .t-${id(i)}{border-color:var(--accent)}`).join("\n");

const html = `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>milkOS: familiar plus one new thing</title>
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
<header><div><h1>milkOS: familiar, plus one new thing</h1><div class="sub">Each screen is the milkOS 1.0 desktop you picked (menu bar, dock, windows) with one small addition, marked NEW. They are add-ons, not replacements, so they can be mixed and shipped one at a time. Each is still drawable with rectangles, text and alpha. Use Prev / Next or tap a thumbnail.</div></div>
${SLIDES.map((s, i) => `<div class="nav n-${id(i)}"><label class="btn" for="r-${id(i - 1)}" role="button" tabindex="0" aria-label="Previous">&larr; Prev</label><span class="count">${i + 1} / ${N}</span><label class="btn" for="r-${id(i + 1)}" role="button" tabindex="0" aria-label="Next">Next &rarr;</label></div>`).join("")}
</header>
${SLIDES.map((s, i) => `<article class="panel p-${id(i)}"><div class="stagebox"><div class="stagewrap">${svg(s)}</div><div class="cap">${s.n} ${s.name}: drawn 1:1 at 1024×768 and scaled to fit.</div></div>
<aside class="notes"><h2>${s.n}. ${s.name}</h2><p class="tag">${s.tag}</p><span class="cost ${s.cost}">${s.cost}</span>
<h3>What feels familiar</h3><p>${s.fam}</p><h3>What is new</h3><p>${s.nw}</p><h3>Who it helps</h3><p>${s.helps}</p><h3>Cost in a kernel-drawn UI</h3><p>${s.why}</p>
<h3>Main elements</h3><ul class="chips">${s.els.map((e) => `<li>${e}</li>`).join("")}</ul><h3>Font note</h3><p>${s.font}</p></aside></article>`).join("")}
<div class="strip">${SLIDES.map((s, i) => `<label class="thumb t-${id(i)}" for="r-${id(i)}" tabindex="0" role="button" aria-label="Show ${s.name}"><div class="tw">${svg(s)}</div><div class="tl"><b>${s.n}. ${s.name}</b><span>${s.cost}</span></div></label>`).join("")}</div>
<section class="plan"><h2>Design notes</h2>
<p>These all sit on top of the 1.0 desktop, so you do not have to pick one. A good order to build them: <b>E Live capsule</b> (easiest, instantly useful), <b>D Shelf and clipboard</b>, <b>A Tabs everywhere</b>, <b>B Projects</b>, and <b>C Time slider</b> last because it needs the filesystem to keep old versions.</p>
<p style="margin-top:8px">If I had to pick the pair that would make someone say "oh, nice" the first day: <b>Projects</b> (your whole setup follows you) and <b>Tabs everywhere</b> (the window clutter problem, solved the way browsers already taught people).</p></section>
</div>
<script>addEventListener('keydown',function(e){var r=[].slice.call(document.querySelectorAll('.r')),c=r.findIndex(function(x){return x.checked});if(e.key==='ArrowRight')r[(c+1)%r.length].checked=true;else if(e.key==='ArrowLeft')r[(c-1+r.length)%r.length].checked=true;else if((e.key==='Enter'||e.key===' ')&&e.target.tagName==='LABEL'){e.preventDefault();e.target.click()}})</script>
</body></html>`;

const here = dirname(fileURLToPath(import.meta.url));
writeFileSync(join(here, "familiar-plus.html"), html);
console.log("wrote familiar-plus.html", html.length, "bytes");
