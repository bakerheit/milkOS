// Five more radical concepts for milkOS (new ideas about what the screen is for). Run: node docs/ui-concepts/wild-concepts.mjs
// Writes wild-concepts.html. Same rules as the rest: rects, rounded rects, text on the 8x16 grid, circles, alpha. No JS needed.
import { B, T, TC, TR, TRi, icon, lights, drop, C, X2, X3, X4, BD } from "./scenes.mjs";
import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const ESP = "rgba(58,46,40,";
const shadow = (x, y, w, h, r) => [[6, 8, 0.05], [3, 5, 0.07], [0, 3, 0.11]].map(([g, dy, a]) => B(x - g, y - g + dy, w + 2 * g, h + 2 * g, `background:${ESP}${a});border-radius:${r + g}px`)).join("");
const lightsOn = (x, y) => [C.straw, C.honey, C.matcha].map((c, i) => B(x + i * 18, y, 12, 12, `background:${c};border-radius:50%`)).join("");
const win = (x, y, w, h, title, body, { dark = false, shade = true } = {}) =>
  (shade ? shadow(x, y, w, h, 12) : "") + B(x, y, w, h, `background:${dark ? C.esp : C.milk};border-radius:12px;border:1px solid ${dark ? "#4A3C34" : C.sand};overflow:hidden`,
    B(0, 0, w, 32, `background:${dark ? "#4A3C34" : C.cream};border-bottom:1px solid ${dark ? "#4A3C34" : C.sand}`) + lightsOn(13, 9) + TC(0, 8, w, `<b>${title}</b>`, `color:${dark ? C.cream : C.esp}`) + body);
const pill = (x, y, w, txt, on = false, h = 28) => B(x, y, w, h, `background:${on ? C.esp : "rgba(255,253,248,.8)"};border:1px solid ${on ? C.esp : C.sand};border-radius:${h / 2}px`) + TC(x, y + (h - 16) / 2, w, txt, `color:${on ? C.cream : C.esp}`);

const circle = (x, y, d, bg, extra = "") => B(x, y, d, d, `background:${bg};border-radius:50%;${extra}`);
const tri = (x, y, w, hh, color) => B(x, y, 0, 0, `border-left:${w / 2}px solid transparent;border-right:${w / 2}px solid transparent;border-bottom:${hh}px solid ${color}`);
const hline = (x1, x2, y, c, t = 3) => B(Math.min(x1, x2), y - t / 2, Math.abs(x2 - x1) + t, t, `background:${c}`);
const vline = (x, y1, y2, c, t = 3) => B(x - t / 2, Math.min(y1, y2), t, Math.abs(y2 - y1) + t, `background:${c}`);
// orthogonal wire from (x1,y1) to (x2,y2) with the vertical run at xm
const wire = (x1, y1, x2, y2, c, xm = (x1 + x2) / 2) => hline(x1, xm, y1, c) + vline(xm, y1, y2, c) + hline(xm, x2, y2, c);

/* ============ 1. PATCHBAY: Unix pipes you can see and rewire ============ */
function patchbay() {
  let h = B(0, 0, 1024, 768, `background:${C.cream}`);
  for (let x = 170; x < 800; x += 24) for (let y = 56; y < 600; y += 24) h += B(x, y, 2, 2, `background:${C.sand}`);
  h += B(0, 0, 1024, 40, `background:${C.milk};border-bottom:1px solid ${C.sand}`) + drop(14, 12, 15) + T(40, 12, "<b>Patchbay</b>   Patch: Send bike photos") +
    B(560, 6, 72, 28, `background:${C.matcha};border-radius:14px`) + TC(560, 12, 72, "<b>Run</b>") + pill(640, 6, 64, "Stop") + pill(712, 6, 64, "Save") + TR(1010, 12, "Tue 10:41   87%", `color:${C.cocoa}`);
  // palette
  h += B(0, 40, 160, 728, `background:${C.oat};border-right:1px solid ${C.sand}`);
  const group = (y, name, items, c) => T(14, y, name, `color:${C.cocoa}`) + items.map((it, i) => B(10, y + 22 + i * 32, 140, 26, `background:${C.milk};border:1px solid ${C.sand};border-left:5px solid ${c};border-radius:8px`) + T(22, y + 27 + i * 32, it)).join("");
  h += group(56, "SOURCES", ["Folder", "Camera", "Mic", "Clock"], C.matcha) + group(240, "FILTERS", ["Resize", "Denoise", "Only new", "Sort"], C.honey) + group(424, "SINKS", ["Mail", "File", "Speaker", "Screen"], C.straw);
  // nodes
  const node = (x, y, w, hh, title, sub, c, body = "") => shadow(x, y, w, hh, 12) + B(x, y, w, hh, `background:${C.milk};border:1px solid ${C.sand};border-radius:12px;overflow:hidden`, B(0, 0, w, 28, `background:${c}`) + T(10, 6, `<b>${title}</b>`) + T(10, 38, sub, `color:${C.cocoa}`) + body);
  const port = (x, y, c) => circle(x - 7, y - 7, 14, C.milk, `border:3px solid ${c}`);
  const W1 = C.latte, W2 = C.blue;
  h += wire(380, 156, 460, 156, W1) + wire(620, 156, 700, 156, W1);
  h += wire(380, 356, 460, 356, W2) + wire(620, 356, 700, 356, W2);
  // data packets
  [[400, 156], [425, 156], [640, 156], [668, 156], [400, 356], [650, 356]].forEach(([x, y], i) => { h += B(x - 4, y - 5, 10, 10, `background:${i < 4 ? C.straw : C.blue};border-radius:2px`); });
  h += node(210, 110, 170, 92, "Folder", "~/photos/bike", C.matcha, T(10, 58, "12 files", `color:${C.cocoa}`)) + node(460, 110, 160, 92, "Resize", "width 800 px", C.honey, T(10, 58, "12 in, 7 out", `color:${C.cocoa}`)) + node(700, 110, 170, 92, "Mail", "to Mum", C.straw, T(10, 58, "7 of 12 sent", `color:${C.matcha};${BD}`));
  h += node(210, 310, 170, 92, "Mic", "built-in", C.matcha, T(10, 58, "listening", `color:${C.cocoa}`)) + node(460, 310, 160, 92, "Denoise", "strength 60%", C.honey) + node(700, 310, 170, 92, "File", "bike.wav", C.straw, T(10, 58, "0:42 saved", `color:${C.cocoa}`));
  [[380, 156], [620, 156], [380, 356], [620, 356]].forEach(([x, y], i) => { h += port(x, y, i < 2 ? C.latte : C.blue) + port(x + 80, y, i < 2 ? C.latte : C.blue); });
  h += T(404, 128, "12 files", `color:${C.cocoa}`) + T(640, 128, "7 files", `color:${C.cocoa}`) + T(404, 328, "audio", `color:${C.cocoa}`) + T(646, 328, "audio", `color:${C.cocoa}`);
  // inspector
  h += B(800, 40, 224, 728, `background:${C.milk};border-left:1px solid ${C.sand}`, T(16, 16, "SELECTED: Resize", `color:${C.cocoa}`) + T(16, 48, "Width") + B(16, 68, 190, 28, `background:${C.cream};border:1px solid ${C.sand};border-radius:8px`, T(10, 6, "800 px")) +
    T(16, 112, "Keep shape") + B(150, 110, 44, 22, `background:${C.straw};border-radius:11px`, circle(24, 2, 18, C.milk)) + T(16, 150, "Quality") + B(16, 176, 190, 8, `background:${C.sand};border-radius:4px`, B(0, 0, 150, 8, `background:${C.straw};border-radius:4px`)) + T(16, 214, "85", `color:${C.cocoa}`) +
    B(16, 260, 190, 1, `background:${C.sand}`) + T(16, 276, "Takes: images", `color:${C.cocoa}`) + T(16, 296, "Gives: images", `color:${C.cocoa}`) + T(16, 330, "Drag a wire from a dot", `color:${C.cocoa}`) + T(16, 348, "to rewire. Click a wire", `color:${C.cocoa}`) + T(16, 366, "to peek at what flows.", `color:${C.cocoa}`));
  // the same thing as a shell command
  h += B(180, 600, 600, 130, `background:${C.esp};border-radius:12px`, T(16, 14, "The same patch as a command (read only):", `color:${C.latte}`) + T(16, 44, "$ files list ~/photos/bike | resize 800 | mail mum", `color:${C.cream}`) + T(16, 66, "$ mic | denoise 0.6 &gt; bike.wav", `color:${C.cream}`) + T(16, 100, "Every box is a normal program. Wires are pipes.", `color:${C.matcha}`));
  return h;
}

/* ============ 2. MEADOW: the wallpaper is the data ============ */
function meadow() {
  let h = B(0, 0, 1024, 768, `background:linear-gradient(180deg,#BFD8F2 0%,#F4E9D0 62%)`);
  // sun on its arc = the time
  for (let i = 0; i <= 12; i++) { const a = Math.PI * (i / 12), x = 120 + (1 - Math.cos(a)) * 400, y = 230 - Math.sin(a) * 170; h += circle(x - 2, y - 2, 4, "rgba(255,253,248,.7)"); }
  h += circle(436, 36, 140, "rgba(255,253,248,.35)") + circle(470, 70, 72, C.honey);
  h += T(472, 236, "10:41", `color:${C.cocoa}`);
  // clouds = unread mail
  const cloud = (x, y, s) => circle(x, y + 12 * s, 36 * s, "rgba(255,253,248,.92)") + circle(x + 28 * s, y, 48 * s, "rgba(255,253,248,.92)") + circle(x + 68 * s, y + 12 * s, 40 * s, "rgba(255,253,248,.92)") + B(x + 12 * s, y + 30 * s, 80 * s, 22 * s, `background:rgba(255,253,248,.92);border-radius:11px`);
  h += cloud(110, 74, 1) + cloud(640, 100, 0.8) + cloud(830, 56, 1.1);
  // hills
  h += circle(-160, 470, 760, "#CFE0C4") + circle(380, 500, 820, "#B9D3B0") + circle(740, 470, 620, "#A5C79F");
  // pond = disk
  h += B(70, 590, 300, 110, `background:#8FB4DB;border-radius:50%;border:5px solid #7CA3CB`) + B(130, 610, 120, 8, "background:rgba(255,255,255,.5);border-radius:4px");
  // lighthouse = network
  h += B(888, 320, 54, 170, `background:${C.milk};border:1px solid ${C.sand}`) + B(888, 360, 54, 26, `background:${C.straw}`) + B(888, 420, 54, 26, `background:${C.straw}`) + B(880, 300, 70, 24, `background:${C.esp};border-radius:6px`) + circle(900, 276, 30, C.honey) + B(950, 288, 60, 6, "background:rgba(235,182,80,.6);border-radius:3px") + B(830, 288, 52, 6, "background:rgba(235,182,80,.6);border-radius:3px");
  // windmill = cpu
  h += B(654, 388, 22, 120, `background:${C.milk};border:1px solid ${C.sand}`) + vline(665, 340, 436, C.esp, 8) + hline(617, 713, 388, C.esp, 8) + circle(657, 380, 16, C.straw);
  // house = files
  h += B(262, 440, 150, 96, `background:${C.milk};border:1px solid ${C.sand}`) + tri(250, 396, 174, 50, C.straw) + B(324, 470, 28, 66, `background:${C.honey}`) + B(284, 462, 28, 28, `background:#BFD8F2;border:2px solid ${C.cocoa}`) + B(366, 462, 28, 28, `background:#BFD8F2;border:2px solid ${C.cocoa}`);
  // barn = projects
  h += B(450, 470, 130, 82, `background:${C.straw}`) + tri(440, 430, 150, 44, C.cocoa) + B(488, 484, 54, 68, `background:${C.milk}`) + hline(488, 542, 518, C.straw, 3);
  // mailbox
  h += B(186, 556, 6, 56, `background:${C.cocoa}`) + B(168, 538, 44, 26, `background:${C.blue};border-radius:13px 13px 4px 4px`) + B(206, 520, 6, 24, `background:${C.straw}`) + B(206, 520, 18, 10, `background:${C.straw}`);
  // flowers = events
  [[760, 590, 1], [800, 620, 0.7]].forEach(([x, y, s]) => { h += vline(x, y, y + 70 * s, "#6E9B6E", 4) + circle(x - 18 * s, y - 18 * s, 36 * s, C.straw) + circle(x - 8 * s, y - 8 * s, 16 * s, C.honey); });
  // labels
  const tag = (x, y, t) => B(x, y, t.length * 8 + 16, 24, `background:rgba(255,253,248,.88);border:1px solid ${C.sand};border-radius:12px`) + T(x + 8, y + 4, t);
  h += tag(288, 372, "Files") + tag(462, 400, "Projects") + tag(624, 312, "CPU 12%") + tag(852, 244, "Wi-Fi: strong") + tag(110, 504, "Mail (3)") + tag(110, 716, "Disk: 40 GB free") + tag(730, 548, "2:30 Dentist");
  return h;
}

/* ============ 3. VERBS: no apps, you say what you want ============ */
function verbs() {
  let h = B(0, 0, 1024, 768, `background:${C.cream}`);
  h += B(0, 0, 1024, 32, `background:${C.milk};border-bottom:1px solid ${C.sand}`) + drop(14, 9, 13) + T(40, 8, "<b>Verbs</b>") + TR(1010, 8, "Tue 10:41   87%", `color:${C.cocoa}`);
  h += T(112, 70, "What do you want to do?", `color:${C.cocoa}`);
  const chip = (x, y, w, txt, c, fg = C.esp, on = false) => shadow(x, y, w, 56, 16) + B(x, y, w, 56, `background:${c};border:${on ? 3 : 1}px solid ${on ? C.esp : C.sand};border-radius:16px`) + T(x + 16, y + 12, txt, X2 + `color:${fg}`) + TR(x + w - 12, y + 18, "&#9662;", `color:${fg === C.cream ? C.latte : C.cocoa}`);
  // the sentence, built from slots
  h += chip(112, 100, 168, "Send", C.esp, C.cream) + chip(296, 100, 248, "bike.png", C.blue + "55", C.esp, true) + T(560, 112, "to", X2 + `color:${C.cocoa}`) + chip(608, 100, 168, "Mum", C.straw + "55") + T(792, 112, "by", X2 + `color:${C.cocoa}`) + chip(840, 100, 150, "Mail", C.matcha + "66");
  // verb row
  h += T(112, 190, "VERBS", `color:${C.cocoa}`);
  ["Send", "Find", "Make", "Play", "Show", "Fix", "Write", "Print"].forEach((v, i) => { h += pill(112 + i * 104, 214, 96, v, i === 0, 32); });
  // pick things
  h += B(112, 270, 420, 330, `background:${C.milk};border:1px solid ${C.sand};border-radius:16px`, T(18, 14, "PICK A THING   recent and nearby", `color:${C.cocoa}`) +
    [["photo", "bike.png", 1], ["photo", "trail.png", 0], ["photo", "lake.png", 0], ["doc", "todo.txt", 0], ["note", "ideas.md", 0], ["doc", "notes.txt", 0]].map((f, i) => {
      const x = 18 + (i % 3) * 132, y = 48 + Math.floor(i / 3) * 128;
      return (f[2] ? B(x - 6, y - 6, 124, 118, `background:${C.blue}33;border:2px solid ${C.blue};border-radius:14px`) : "") + icon(x + 24, y, 64, f[0], C.cream, C.esp, `border:1px solid ${C.sand}`) + TC(x - 6, y + 74, 124, f[1]);
    }).join("") + T(18, 300, "Type to search, or drop a file here.", `color:${C.cocoa}`));
  // who
  h += B(548, 270, 180, 330, `background:${C.milk};border:1px solid ${C.sand};border-radius:16px`, T(16, 14, "TO WHOM", `color:${C.cocoa}`) + ["Mum", "Sam", "Dad", "Robin", "Me"].map((p, i) => (i === 0 ? B(8, 42 + i * 52, 164, 44, `background:${C.straw}44;border-radius:10px`) : "") + circle(18, 50 + i * 52, 28, [C.straw, C.blue, C.matcha, C.honey, C.lav][i]) + T(58, 58 + i * 52, p)).join(""));
  // preview + do it
  h += B(744, 270, 248, 330, `background:${C.milk};border:2px solid ${C.esp};border-radius:16px`, T(16, 14, "THIS WILL", `color:${C.cocoa}`) + T(16, 44, "Email bike.png") + T(16, 62, "(2.1 MB) to Mum") + T(16, 80, "from milk@milk.", `color:${C.cocoa}`) + icon(16, 110, 64, "photo", C.cream, C.esp, `border:1px solid ${C.sand}`) + T(90, 124, "bike.png", `color:${C.cocoa}`) + T(90, 142, "800x600", `color:${C.cocoa}`) +
    B(16, 230, 216, 48, `background:${C.esp};border-radius:24px`) + TC(16, 246, 216, "<b>Do it   &#9166;</b>", `color:${C.cream}`) + T(16, 290, "Undo for 30 seconds", `color:${C.cocoa}`));
  // history
  h += T(112, 626, "DONE TODAY", `color:${C.cocoa}`) + T(112, 650, "&#10003; Found notes.txt       &#10003; Printed todo.txt       &#10003; Played Cream Soda Skies", `color:${C.cocoa}`) +
    T(112, 690, "No app windows are open. Verbs borrows the right program for each job and hands you back to this screen.", `color:${C.cocoa}`);
  return h;
}

/* ============ 4. FRIDGE DOOR: the milk-brand kitchen metaphor ============ */
function fridge() {
  let h = B(0, 0, 1024, 768, `background:#F1E4CB`) + B(0, 700, 1024, 68, `background:#E3D2B1`);
  // fridge body
  h += shadow(200, 28, 624, 716, 36) + B(200, 28, 624, 716, `background:linear-gradient(90deg,#FFFDF8,#F6F0E2);border:2px solid ${C.sand};border-radius:36px`);
  h += B(200, 262, 624, 6, `background:${C.sand}`);
  // handles
  h += B(226, 108, 14, 100, `background:${C.latte};border-radius:7px`) + B(226, 330, 14, 160, `background:${C.latte};border-radius:7px`);
  // freezer panel: status
  h += B(300, 60, 420, 170, `background:${C.esp};border-radius:20px`, T(24, 18, "4&deg;", X4 + `color:${C.cream}`) + T(150, 22, "Tue 21 Oct", `color:${C.latte}`) + T(150, 44, "10:41", X3 + `color:${C.cream}`) + T(24, 100, "Wi-Fi: home-5G", `color:${C.sand}`) + T(24, 122, "All systems cool", `color:${C.matcha}`));
  // milk carton = battery
  h += B(740, 70, 56, 150, `background:${C.milk};border:2px solid ${C.esp};border-radius:6px`, B(0, 60, 52, 86, `background:${C.blue}55`) + TC(0, 20, 52, "87%", `color:${C.esp}`)) + tri(740, 44, 56, 26, C.milk) + B(748, 40, 40, 8, `background:${C.esp};border-radius:2px`);
  // magnets: apps
  [["folder", C.honey, "Files"], ["term", C.esp, "Shell"], ["globe", C.blue, "Web"], ["music", C.straw, "Music"], ["photo", C.cream, "Photos"], ["gear", C.latte, "Settings"]].forEach((m, i) => {
    const x = 290 + (i % 3) * 140, y = 290 + Math.floor(i / 3) * 112;
    h += circle(x, y, 76, "rgba(58,46,40,.12)", "") + circle(x - 2, y - 4, 76, m[1], `border:3px solid ${C.milk}`) + icon(x + 12, y + 8, 48, m[0], "transparent", m[0] === "term" ? C.cream : C.esp) + TC(x - 20, y + 78, 116, m[2], `color:${C.cocoa}`);
  });
  // sticky notes = notifications
  const sticky = (x, y, c, l1, l2) => B(x + 3, y + 5, 150, 100, "background:rgba(58,46,40,.10)") + B(x, y, 150, 100, `background:${c}`, T(12, 12, `<b>${l1}</b>`) + T(12, 34, l2, `color:${C.cocoa}`) + T(12, 72, "peel to dismiss", `color:${C.cocoa}`));
  h += sticky(640, 296, "#FBE59A", "Dentist 2:30", "12 Elm St") + sticky(640, 410, "#F8C6CF", "Call Mum", "she emailed") + sticky(470, 520, "#CFE0F6", "Buy milk!", "(obviously)");
  // photo magnet
  h += B(296, 526, 150, 130, `background:${C.milk};border:1px solid ${C.sand}`, B(10, 10, 128, 90, `background:${C.peach}`) + circle(40, 28, 30, C.honey) + B(10, 70, 128, 30, `background:${C.matcha}`) + TC(0, 104, 148, "bike ride", `color:${C.cocoa}`)) + circle(358, 516, 22, C.straw, `border:3px solid ${C.milk}`);
  // shopping list
  h += B(640, 530, 140, 150, `background:${C.milk};border:1px solid ${C.sand}`, T(10, 10, "<b>Shopping</b>") + T(10, 34, "Milk") + T(10, 52, "Eggs") + T(10, 70, "Rice") + T(10, 88, "Jam") + [0, 1, 2, 3].map((i) => hline(8, 128, 46 + i * 18, C.sand, 1)).join("")) + circle(700, 520, 22, C.blue, `border:3px solid ${C.milk}`);
  // drawer
  h += shadow(240, 676, 544, 52, 16) + B(240, 676, 544, 52, `background:${C.oat};border:2px solid ${C.sand};border-radius:16px`) + B(452, 690, 120, 14, `background:${C.latte};border-radius:7px`) + T(262, 694, "Crisper: your files", `color:${C.cocoa}`) + TR(764, 694, "pull to open", `color:${C.cocoa}`);
  h += T(24, 40, "Magnets open apps.", `color:${C.cocoa}`) + T(24, 60, "Sticky notes are", `color:${C.cocoa}`) + T(24, 78, "notifications.", `color:${C.cocoa}`) + T(24, 110, "The carton is your", `color:${C.cocoa}`) + T(24, 128, "battery.", `color:${C.cocoa}`);
  return h;
}

/* ============ 5. ONE THING: tasks, not apps; interruptions get parked ============ */
function onething() {
  let h = B(0, 0, 1024, 768, `background:${C.cream}`);
  h += B(0, 0, 1024, 32, `background:${C.milk};border-bottom:1px solid ${C.sand}`) + drop(14, 9, 13) + T(40, 8, "<b>One Thing</b>   Task 2 of 5 today") + [0, 1, 2, 3, 4].map((i) => circle(300 + i * 18, 11, 10, i < 1 ? C.matcha : i === 1 ? C.straw : C.sand)).join("") + TR(1010, 8, "10:41   87%", `color:${C.cocoa}`);
  // current task header
  h += T(40, 52, "NOW", `color:${C.cocoa}`) + T(40, 74, "Write the UI spec", X3 + BD) + T(40, 130, "Focus: 25:00 left   started 10:16", `color:${C.cocoa}`) + B(40, 156, 700, 10, `background:${C.sand};border-radius:5px`, B(0, 0, 330, 10, `background:${C.straw};border-radius:5px`));
  // the work area: one app, nearly chromeless
  h += shadow(40, 188, 700, 450, 14) + B(40, 188, 700, 450, `background:${C.milk};border:1px solid ${C.sand};border-radius:14px;overflow:hidden`,
    B(0, 0, 700, 30, `background:${C.cream};border-bottom:1px solid ${C.sand}`) + T(14, 7, "Editor   spec.md", `color:${C.cocoa}`) + TR(688, 7, "needs: Files, Docs", `color:${C.cocoa}`) +
    ["# milkOS 1.0 spec", "", "## Menu bar", "Height 28. Glass, alpha 0.55.", "", "## Dock", "Height 64, icons 48, gap 12.", "Centred, 12 px above the bottom.", "", "## Window chrome", "Radius 12, title bar 32.", "Lights d12, gap 8."].map((l, i) => T(24, 50 + i * 22, l, l.startsWith("#") ? BD : "")).join("") + B(24 + 8 * 18, 50 + 11 * 22, 8, 16, `background:${C.esp}`));
  // right column: queue
  h += B(764, 52, 230, 300, `background:${C.oat};border:1px solid ${C.sand};border-radius:14px`, T(16, 14, "NEXT", `color:${C.cocoa}`) +
    [["Reply to Sam", "5 min"], ["Fix the allocator", "1 hour"], ["Lunch", "12:30"], ["Bike ride", "5:00"]].map((t, i) => B(10, 40 + i * 62, 210, 54, `background:${C.milk};border:1px solid ${C.sand};border-radius:10px`, T(12, 8, `<b>${i + 1}. ${t[0]}</b>`) + T(12, 28, t[1], `color:${C.cocoa}`))).join(""));
  // parked
  h += B(764, 368, 230, 270, `background:${C.honey}22;border:1px dashed ${C.honey};border-radius:14px`, T(16, 14, "PARKED", `color:${C.cocoa}`) + T(16, 34, "held until your 10:50 break", `color:${C.cocoa}`) +
    [["Mail from Mum", C.straw], ["Update available", C.blue], ["Ping from Robin", C.matcha]].map((p, i) => B(12, 62 + i * 62, 206, 50, `background:${C.milk};border:1px solid ${C.sand};border-left:6px solid ${p[1]};border-radius:10px`, T(14, 8, `<b>${p[0]}</b>`) + T(14, 26, "tap to let through", `color:${C.cocoa}`))).join(""));
  // actions
  h += B(40, 660, 160, 48, `background:${C.matcha};border-radius:24px`) + TC(40, 676, 160, "<b>Done &#10003;</b>") + pill(214, 668, 150, "Swap task", false, 32) + pill(376, 668, 150, "Park this", false, 32) + T(560, 676, "Esc = park everything and breathe", `color:${C.cocoa}`);
  h += T(40, 728, "Apps open only as tools for the task and close with it. There is no desktop to wander off to.", `color:${C.cocoa}`);
  return h;
}

const SLIDES = [
  { id: "patchbay", n: "1", name: "Patchbay", tag: "Unix pipes you can see and rewire", draw: patchbay, cost: "medium",
    idea: "The desktop is a patch bay. Every program is a box with input and output dots, and you drag wires between them: Folder to Resize to Mail. Data visibly flows down the wires, and the same patch is shown as the one-line shell command it really is.",
    new_: "A GUI that is honest about what Linux pipes are. Nothing hidden behind a window: you build, watch and edit the thing that is happening.",
    why: "Boxes, dots and orthogonal wires are plain rectangles, so drawing is cheap. The real work is the plumbing: a process spawner, pipes, a typed port system (images, audio, text) and a scheduler that moves data. That plumbing is something a Linux-style OS wants anyway.",
    els: ["Patch canvas", "Node palette (sources, filters, sinks)", "Typed ports + wires", "Live data counters", "Inspector for settings", "Command view of the patch", "Run / stop / save"],
    font: "Unifont is a good match for the node text and the command view. Wires and ports carry the look." },
  { id: "meadow", n: "2", name: "Meadow", tag: "The wallpaper is the data", draw: meadow, cost: "medium",
    idea: "A calm scene that is also your dashboard. The sun's position is the time. Clouds are unread mail, the pond is free disk, the windmill spins with the CPU, the lighthouse is the network, flowers are today's events. The house opens Files, the barn opens Projects.",
    new_: "Information shown as scenery you read at a glance instead of widgets you read as text. Apps are places you walk into.",
    why: "A scene made of circles, rects and triangles is cheap to draw, and each scene element is just a gauge bound to one number. The cost is art: it needs good taste and a handful of states (day, dusk, night). Hit testing is simple shapes.",
    els: ["Scene with gauges", "Sun-arc clock", "Buildings as launchers", "Mail clouds, disk pond, CPU windmill", "Event flowers", "Hover labels", "Day / dusk / night skies"],
    font: "Almost no text, so Unifont hardly matters. The labels are a hover feature." },
  { id: "verbs", n: "3", name: "Verbs", tag: "No apps. You build a sentence.", draw: verbs, cost: "easy",
    idea: "The whole interface is one sentence with slots: Send [bike.png] to [Mum] by [Mail]. Pick a verb, then a thing, then a person, and a preview says exactly what will happen before you press Do it. No app windows are open; the OS borrows the right program for the job.",
    new_: "Verb-first instead of app-first. People think \"send this to Mum\", not \"open Mail, attach, find the file\".",
    why: "It is a form: chips, a picker grid and a preview. The cost is the verb registry (each verb says what it accepts and which program does it), which is a small table. Undo for 30 seconds needs a little bookkeeping.",
    els: ["Sentence builder", "Verb row", "Thing picker (recent and nearby)", "Person picker", "Plain-English preview", "Do it / Undo", "History"],
    font: "The sentence at 2x Unifont looks chunky but friendly. A rounded sans would make it feel more like a product." },
  { id: "fridge", n: "4", name: "Fridge Door", tag: "A kitchen, because it is milk", draw: fridge, cost: "easy",
    idea: "The home screen is a fridge door. App magnets, sticky-note notifications you peel off to dismiss, a photo and a shopping list held by magnets, a milk carton that is your battery, a Crisper drawer that is your files. The freezer panel shows temperature as the clock and status.",
    new_: "A fully domestic, brand-led metaphor. It makes milkOS instantly recognisable and a bit silly in the best way.",
    why: "Rounded rects, circles and drop shadows. Magnets and notes are just draggable cards on a background. The only fiddly parts are drag and drop and the drawer animation, which can simply snap open.",
    els: ["Magnet app launchers", "Sticky-note notifications", "Photo and list magnets", "Milk-carton battery", "Freezer status panel", "Crisper files drawer", "Drag to rearrange"],
    font: "Unifont looks like handwriting-lite on sticky notes. A handwriting font would make the notes charming." },
  { id: "onething", n: "5", name: "One Thing", tag: "Tasks, not apps; interruptions get parked", draw: onething, cost: "easy",
    idea: "The OS shows only the task you are doing now, with its tools open and nothing else. What is next waits in a queue. Interruptions (mail, updates, pings) are parked and held until your break instead of popping up. Done moves you to the next task.",
    new_: "The unit of the screen is a task with a timer, not a window. The desktop you could wander off to simply does not exist.",
    why: "A task is a name, a timer and a list of tools (apps it needs). The UI is a header, one big window and two side lists. The parking logic is a notification filter with a release time. This is the cheapest concept here.",
    els: ["Current task header + timer", "One main work area", "Next queue", "Parked tray", "Done / Swap / Park", "Day progress dots", "Tool list per task"],
    font: "Unifont works. The big task title at 3x is the main visual." },
];

/* ------------------------------ page ------------------------------ */
const N = SLIDES.length;
const id = (i) => `n${((i % N) + N) % N}`;
const svg = (s) => `<svg class="scene" viewBox="0 0 1024 768" role="img" aria-label="Mockup of ${s.name}"><foreignObject x="0" y="0" width="1024" height="768"><div class="stage">${s.draw()}</div></foreignObject></svg>`;
const rules = SLIDES.map((s, i) => `#r-${id(i)}:checked ~ .wrap .p-${id(i)}{display:grid}\n#r-${id(i)}:checked ~ .wrap .n-${id(i)}{display:flex}\n#r-${id(i)}:checked ~ .wrap .t-${id(i)}{border-color:var(--accent)}`).join("\n");

const html = `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>milkOS: 5 more ideas</title>
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
<header><div><h1>milkOS: 5 totally different ideas</h1><div class="sub">Not new layouts of a desktop: five different answers to what the screen is for (pipes, scenery, sentences, a kitchen, a single task). Each is still drawable by the kernel with rectangles, rounded rects, text on the 8×16 grid, circles and alpha. Use Prev / Next or tap a thumbnail.</div></div>
${SLIDES.map((s, i) => `<div class="nav n-${id(i)}"><label class="btn" for="r-${id(i - 1)}" role="button" tabindex="0" aria-label="Previous">&larr; Prev</label><span class="count">${i + 1} / ${N}</span><label class="btn" for="r-${id(i + 1)}" role="button" tabindex="0" aria-label="Next">Next &rarr;</label></div>`).join("")}
</header>
${SLIDES.map((s, i) => `<article class="panel p-${id(i)}"><div class="stagebox"><div class="stagewrap">${svg(s)}</div><div class="cap">${s.n} ${s.name}: drawn 1:1 at 1024×768 and scaled to fit.</div></div>
<aside class="notes"><h2>${s.n}. ${s.name}</h2><p class="tag">${s.tag}</p><span class="cost ${s.cost}">${s.cost}</span>
<h3>Key idea</h3><p>${s.idea}</p><h3>What is new about it</h3><p>${s.new_}</p><h3>Cost in a kernel-drawn UI</h3><p>${s.why}</p>
<h3>Main elements</h3><ul class="chips">${s.els.map((e) => `<li>${e}</li>`).join("")}</ul><h3>Font note</h3><p>${s.font}</p></aside></article>`).join("")}
<div class="strip">${SLIDES.map((s, i) => `<label class="thumb t-${id(i)}" for="r-${id(i)}" tabindex="0" role="button" aria-label="Show ${s.name}"><div class="tw">${svg(s)}</div><div class="tl"><b>${s.n}. ${s.name}</b><span>${s.cost}</span></div></label>`).join("")}</div>
<section class="plan"><h2>Design notes</h2>
<p><b>Most milkOS-ish: 1 Patchbay.</b> It turns the Linux idea everyone loves (small programs joined by pipes) into something you can see and touch, and the plumbing it needs is plumbing the OS needs anyway.</p>
<p style="margin-top:8px"><b>Most radical: 3 Verbs.</b> It deletes the idea of an app from the user's view. It is also the cheapest way to try something truly different: a form, a verb table and a preview.</p>
<p style="margin-top:8px"><b>Best for the brand: 4 Fridge Door.</b> Nobody else will ship a milk carton as a battery meter. <b>2 Meadow</b> and <b>5 One Thing</b> work well as modes layered on a normal desktop (an ambient screensaver, a focus mode) rather than as the whole OS.</p></section>
</div>
<script>addEventListener('keydown',function(e){var r=[].slice.call(document.querySelectorAll('.r')),c=r.findIndex(function(x){return x.checked});if(e.key==='ArrowRight')r[(c+1)%r.length].checked=true;else if(e.key==='ArrowLeft')r[(c-1+r.length)%r.length].checked=true;else if((e.key==='Enter'||e.key===' ')&&e.target.tagName==='LABEL'){e.preventDefault();e.target.click()}})</script>
</body></html>`;

const here = dirname(fileURLToPath(import.meta.url));
writeFileSync(join(here, "wild-concepts.html"), html);
console.log("wrote wild-concepts.html", html.length, "bytes");
