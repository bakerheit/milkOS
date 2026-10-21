// Four new-paradigm concepts for milkOS (not skins of the first ten). Run: node docs/ui-concepts/new-concepts.mjs
// Writes new-concepts.html. Same rules as the rest: rects, rounded rects, text on the 8x16 grid, circles, alpha. No JS needed.
import { B, T, TC, TR, TRi, icon, lights, drop, C, X2, X3, BD } from "./scenes.mjs";
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

/* ============ 1. REWIND: the OS is a timeline you can scrub ============ */
function rewind() {
  let h = B(0, 0, 1024, 768, `background:${C.cream}`);
  // top bar
  h += B(0, 0, 1024, 44, `background:${C.milk};border-bottom:1px solid ${C.sand}`) + drop(16, 14, 15) + T(44, 14, "<b>Rewind</b>") + pill(130, 8, 148, "Today, Tue 21 Oct") +
    B(300, 8, 420, 28, `background:${C.cream};border:1px solid ${C.sand};border-radius:14px`) + T(316, 14, "Search your past: &quot;kernel notes&quot;", `color:${C.cocoa}`) + TR(1008, 14, "Wi-Fi   87%", `color:${C.cocoa}`);
  // left: moments
  h += B(0, 44, 248, 536, `background:${C.oat};border-right:1px solid ${C.sand}`) + T(16, 58, "MOMENTS", `color:${C.cocoa}`);
  const moments = [["9:40 AM", "Build passed", "milkOS", 0], ["9:12 AM", "Opened kernel notes", "milkOS", 1], ["8:55 AM", "Started the day", "", 0], ["Mon 4:05 PM", "Bike repair list", "Personal", 0], ["Mon 1:30 PM", "Wrote UI spec", "milkOS", 0], ["Sun 7:10 PM", "Photos from the ride", "Personal", 0]];
  moments.forEach((m, i) => {
    const y = 84 + i * 76;
    h += (m[3] ? B(8, y - 4, 232, 68, `background:${C.straw}33;border-radius:10px;border:1px solid ${C.straw}`) : "") +
      B(20, y + 6, 10, 10, `background:${m[3] ? C.straw : C.latte};border-radius:50%`) + T(40, y, `<b>${m[1]}</b>`) + T(40, y + 20, m[0], `color:${C.cocoa}`) + (m[2] ? T(40, y + 38, m[2], `color:${C.cocoa}`) : "");
  });
  // centre: the past, shown as it was
  h += B(272, 64, 728, 440, `background:linear-gradient(160deg,#F9EBD3,#EBD7BB);border-radius:14px;border:2px solid ${C.straw};overflow:hidden`,
    B(0, 0, 728, 28, `background:rgba(255,253,248,.7)`) + T(12, 6, "<b>Files</b>   File   Edit   View") + TR(716, 6, "9:12 AM", `color:${C.cocoa}`) +
    win(28, 48, 360, 250, "kernel notes.md", T(16, 46, "# milkOS kernel") + T(16, 66, "- boot on qemu virt") + T(16, 84, "- UART first, then display") + T(16, 102, "- heap next", `color:${C.cocoa}`) + T(16, 140, "TODO: interrupts", `background:${C.honey}55`), { shade: true }) +
    win(240, 150, 440, 250, "milk: sh", T(14, 46, "$ cargo build", `color:${C.cream}`) + T(14, 64, "   Compiling milkos-kernel", `color:${C.latte}`) + T(14, 82, "error: no global allocator", `color:${C.straw}`) + T(14, 100, "$ _", `color:${C.cream}`), { dark: true }));
  // action bar under the viewer
  h += pill(272, 516, 190, "3 files changed since", false, 32) + T(474, 524, "Drag a window or file out", `color:${C.cocoa}`) +
    B(790, 516, 210, 32, `background:${C.esp};border-radius:16px`) + TC(790, 524, 210, "<b>Restore this moment</b>", `color:${C.cream}`);
  // bottom: the scrubber
  h += B(0, 580, 1024, 188, `background:${C.milk};border-top:1px solid ${C.sand}`);
  h += T(16, 590, "TIMELINE", `color:${C.cocoa}`);
  const days = ["Sat", "Sun", "Mon", "Tue (today)"];
  days.forEach((d, i) => { const x = 24 + i * 246; h += B(x, 628, 238, 44, `background:${C.cream};border-radius:8px;border:1px solid ${C.sand}`) + T(x + 6, 608, d, `color:${C.cocoa}`); });
  const blocks = [[30, 60, C.blue], [100, 40, C.straw], [280, 80, C.honey], [370, 120, C.blue], [510, 50, C.matcha], [620, 90, C.blue], [740, 40, C.straw], [800, 110, C.blue], [930, 54, C.honey]];
  blocks.forEach(([x, w, c]) => { h += B(x, 636, w, 28, `background:${c};border-radius:6px;opacity:.85`); });
  [[420, 0], [540, 0], [690, 0], [820, 0], [890, 0]].forEach(([x]) => { h += B(x, 672, 2, 10, `background:${C.cocoa}`); });
  h += B(840, 622, 3, 82, `background:${C.straw}`) + B(830, 702, 24, 24, `background:${C.straw};border-radius:50%;border:3px solid ${C.milk}`) + B(864, 704, 96, 22, `background:${C.esp};border-radius:11px`) + TC(864, 707, 96, "Tue 9:12", `color:${C.cream}`);
  h += T(24, 746, "&#9632; code   ", `color:${C.blue}`) + T(112, 746, "&#9632; web   ", `color:${C.straw}`) + T(192, 746, "&#9632; writing   ", `color:${C.honey}`) + T(300, 746, "&#9632; media", `color:${C.matcha}`) + TR(1000, 746, "Drag to scrub   &larr; &rarr; step   Space to play back", `color:${C.cocoa}`);
  return h;
}

/* ============ 2. CANVAS: one endless zoomable surface ============ */
function canvas() {
  let h = B(0, 0, 1024, 768, `background:${C.cream}`);
  for (let x = 20; x < 1024; x += 32) for (let y = 40; y < 768; y += 32) h += B(x, y, 2, 2, `background:${C.sand}`);
  // connectors
  h += B(262, 252, 160, 2, `background:${C.latte}`) + B(512, 336, 2, 134, `background:${C.latte}`);
  // cluster labels (far-zoom text drawn at 2x, never shrunk)
  h += T(40, 70, "SCHOOL", X2 + `color:${C.latte}`) + T(520, 70, "milkOS", X2 + `color:${C.latte}`) + T(520, 480, "PERSONAL", X2 + `color:${C.latte}`);
  // school: far zoom -> cards
  const card = (x, y, w, hh, title, sub, c, ic) => shadow(x, y, w, hh, 14) + B(x, y, w, hh, `background:${C.milk};border:1px solid ${C.sand};border-radius:14px`, B(0, 0, 6, hh, `background:${c};border-radius:14px 0 0 14px`) + icon(18, hh / 2 - 22, 44, ic, C.cream, C.esp, `border:1px solid ${C.sand}`) + T(74, hh / 2 - 18, `<b>${title}</b>`) + T(74, hh / 2 + 2, sub, `color:${C.cocoa}`));
  h += card(40, 120, 220, 76, "Essay draft", "edited 2h ago", C.honey, "doc") + card(40, 216, 220, 76, "Reading list", "12 items", C.honey, "note") + card(300, 216, 190, 76, "Maths set 4", "due Fri", C.straw, "calc");
  // milkOS: near zoom -> real windows
  h += win(520, 112, 330, 220, "kernel/main.rs", [..."fn kmain() {|  uart::init();|  mem::init();|  display::init();|  println!(&quot;up&quot;);|}"].join("").split("|").map((l, i) => T(14, 44 + i * 18, l)).join(""));
  h += win(868, 150, 140, 120, "sh", T(10, 42, "$ _", `color:${C.cream}`), { dark: true });
  // personal
  h += card(520, 528, 220, 76, "Bike photos", "48 photos", C.matcha, "photo") + card(760, 528, 220, 76, "Trip plan", "Lake Erie", C.blue, "cal");
  // zoom level indicator + breadcrumb
  h += B(0, 0, 1024, 36, `background:rgba(255,253,248,.85);border-bottom:1px solid ${C.sand}`) + drop(16, 11, 14) + T(42, 10, "<b>Canvas</b>   Home &rsaquo; Everything") + TR(1008, 10, "Wi-Fi   87%   Tue 10:41", `color:${C.cocoa}`);
  h += shadow(24, 690, 214, 52, 26) + B(24, 690, 214, 52, `background:rgba(255,253,248,.92);border:1px solid ${C.sand};border-radius:26px`) + T(46, 706, "Zoom") + B(96, 713, 110, 6, `background:${C.sand};border-radius:3px`, B(0, 0, 44, 6, `background:${C.straw};border-radius:3px`)) + B(134, 706, 18, 18, `background:${C.straw};border-radius:50%;border:3px solid ${C.milk}`) + T(212, 706, "40%", `color:${C.cocoa}`);
  // minimap
  h += shadow(780, 664, 220, 96, 12) + B(780, 664, 220, 96, `background:rgba(255,253,248,.92);border:1px solid ${C.sand};border-radius:12px`, B(14, 14, 60, 40, `background:${C.honey}66;border-radius:4px`) + B(90, 14, 80, 38, `background:${C.blue}66;border-radius:4px`) + B(90, 58, 100, 22, `background:${C.matcha}66;border-radius:4px`) + B(78, 8, 128, 66, `border:2px solid ${C.straw};border-radius:4px`));
  // levels legend
  h += T(260, 706, "Zoom out: windows become cards.  Zoom in: cards become windows.", `color:${C.cocoa}`);
  return h;
}

/* ============ 3. SCROLL: a notebook shell, every command leaves a live card ============ */
function scroll() {
  let h = B(0, 0, 1024, 768, `background:${C.cream}`);
  h += B(0, 0, 1024, 32, `background:${C.esp}`) + T(14, 8, "<b>Scroll</b>   notebook: milkOS work", `color:${C.cream}`) + TR(1008, 8, "Tue 10:41   87%", `color:${C.sand}`);
  const cell = (y, n, cmd, hh, body, pinned) => B(24, y, 700, hh, `background:${C.milk};border:1px solid ${C.sand};border-radius:12px;overflow:hidden`,
    B(0, 0, 700, 30, `background:${C.oat};border-bottom:1px solid ${C.sand}`) + T(12, 7, `[${n}]`, `color:${C.cocoa}`) + T(48, 7, `<b>${cmd}</b>`) + TR(688, 7, pinned ? "pinned   &#9679;" : "pin   &#9675;   &#8943;", `color:${pinned ? C.straw : C.cocoa}`) + body);
  h += cell(48, 1, "ls ~/photos", 160,
    [0, 1, 2, 3, 4, 5].map((i) => B(14 + i * 108, 44, 98, 76, `background:${[C.peach, C.matcha, C.blue, C.honey, C.lav, C.straw][i]};border-radius:8px`) + TC(14 + i * 108, 124, 98, ["bike.png", "trail.png", "lake.png", "sun.png", "dusk.png", "tree.png"][i], `color:${C.cocoa}`)).join("") + T(14, 142, "6 images, 12 MB   (click to open, drag to copy)", `color:${C.cocoa}`));
  h += cell(228, 2, "disk", 124,
    [["/", 70, C.matcha, "70 GB free"], ["/home", 160, C.honey, "40 GB free"], ["/tmp", 18, C.matcha, "1 GB used"]].map((d, i) => T(14, 44 + i * 26, d[0].padEnd(7)) + B(80, 46 + i * 26, 420, 14, `background:${C.sand};border-radius:7px`, B(0, 0, d[1] * 2.4, 14, `background:${d[2]};border-radius:7px`)) + T(516, 44 + i * 26, d[3], `color:${C.cocoa}`)).join(""), true);
  h += cell(372, 3, "edit todo.txt", 150,
    B(14, 42, 672, 96, `background:${C.cream};border:1px solid ${C.sand};border-radius:8px`) + T(24, 52, "[x] draft the UI spec") + T(24, 70, "[x] pick a look") + T(24, 88, "[ ] fix the allocator") + B(168, 88, 8, 16, `background:${C.esp}`) + T(24, 106, "[ ] boot logo", `color:${C.cocoa}`));
  h += cell(542, 4, "play cream-soda-skies", 96,
    icon(14, 40, 44, "music", C.straw, C.esp) + T(70, 40, "<b>Cream Soda Skies</b>") + T(70, 60, "The Lactose Intolerants", `color:${C.cocoa}`) + B(330, 50, 300, 8, `background:${C.sand};border-radius:4px`, B(0, 0, 120, 8, `background:${C.straw};border-radius:4px`)) + T(640, 44, "1:12", `color:${C.cocoa}`));
  // prompt
  h += B(24, 660, 700, 44, `background:${C.milk};border:2px solid ${C.esp};border-radius:12px`, T(14, 14, "[5] ", `color:${C.cocoa}`) + T(48, 14, "cp ~/photos/ba") + B(166, 14, 8, 16, `background:${C.esp}`)) +
    pill(24, 714, 130, "bike.png", false, 28) + pill(162, 714, 120, "backup/", false, 28) + T(300, 720, "Tab to complete   Up for history   Ctrl-P palette", `color:${C.cocoa}`);
  // right rail: pinned live cards
  h += B(748, 48, 252, 672, `background:${C.oat};border:1px solid ${C.sand};border-radius:12px`, T(14, 12, "PINNED (live)", `color:${C.cocoa}`) +
    B(12, 40, 228, 130, `background:${C.milk};border:1px solid ${C.sand};border-radius:10px`, T(12, 10, "disk", `color:${C.cocoa}`) + B(12, 40, 200, 12, `background:${C.sand};border-radius:6px`, B(0, 0, 70, 12, `background:${C.matcha};border-radius:6px`)) + B(12, 66, 200, 12, `background:${C.sand};border-radius:6px`, B(0, 0, 150, 12, `background:${C.honey};border-radius:6px`)) + T(12, 96, "updates every 5s", `color:${C.latte}`)) +
    B(12, 182, 228, 100, `background:${C.milk};border:1px solid ${C.sand};border-radius:10px`, T(12, 10, "clock", `color:${C.cocoa}`) + T(12, 34, "10:41:07", X3 + BD)) +
    B(12, 294, 228, 120, `background:${C.milk};border:1px solid ${C.sand};border-radius:10px`, T(12, 10, "build", `color:${C.cocoa}`) + T(12, 34, "&#10003; passed 2m ago", `color:${C.matcha};${BD}`) + T(12, 56, "0 warnings") + T(12, 84, "re-run: [3]", `color:${C.cocoa}`)) +
    T(14, 436, "Anything you run can be pinned.", `color:${C.cocoa}`) + T(14, 454, "Pins stay live and survive", `color:${C.cocoa}`) + T(14, 472, "closing the notebook.", `color:${C.cocoa}`));
  return h;
}

/* ============ 4. DIAL: modes that reshape the OS + radial menus ============ */
function dial() {
  let h = B(0, 0, 1024, 768, `background:linear-gradient(180deg,#E9EEF9,#DCE4F6)`);
  h += B(-120, 520, 560, 560, `border-radius:50%;background:rgba(127,147,201,.22)`) + B(700, -160, 480, 480, `border-radius:50%;background:rgba(255,253,248,.4)`);
  // mode header
  h += B(0, 0, 1024, 32, `background:rgba(255,253,248,.7);border-bottom:1px solid rgba(255,255,255,.8)`) + drop(14, 9, 13) + T(40, 8, "<b>Build mode</b>") + TR(1010, 8, "Wi-Fi   87%   Tue 10:41", `color:${C.cocoa}`);
  h += T(48, 56, "Build", X3 + BD + `color:${C.esp}`) + T(50, 108, "Editor, shell and docs up front. Chat and mail held until Rest.", `color:${C.cocoa}`);
  // cards for this mode
  const card = (x, y, w, hh, title, body, c) => shadow(x, y, w, hh, 18) + B(x, y, w, hh, `background:rgba(255,253,248,.82);border:1px solid rgba(255,255,255,.9);border-radius:18px;overflow:hidden`, B(0, 0, w, 6, `background:${c}`) + T(16, 16, `<b>${title}</b>`) + body);
  h += card(48, 150, 330, 250, "Editor: kernel/main.rs", T(16, 44, "fn kmain() {") + T(16, 62, "  uart::init();") + T(16, 80, "  mem::init();") + T(16, 98, "  display::init();") + T(16, 116, "}") + T(16, 160, "Last build: passed", `color:${C.matcha};${BD}`) + T(16, 184, "3 TODOs", `color:${C.cocoa}`), C.blue);
  h += card(396, 150, 270, 250, "Shell", B(0, 38, 270, 212, `background:${C.esp}`) + T(14, 52, "$ cargo build", `color:${C.cream}`) + T(14, 70, "Finished in 2.1s", `color:${C.matcha}`) + T(14, 88, "$ _", `color:${C.cream}`), C.blue);
  h += card(684, 150, 290, 120, "Docs", T(16, 44, "milkOS UI spec") + T(16, 64, "Rust book: ownership", `color:${C.cocoa}`), C.blue);
  h += card(684, 288, 290, 112, "Held until Rest", T(16, 44, "3 messages, 1 email") + T(16, 64, "Nothing urgent from Mum", `color:${C.cocoa}`) + T(16, 84, "[Let through]", `color:${C.straw}`), C.latte);
  // radial context menu over a file
  const cx = 520, cy = 505, R = 84;
  h += icon(cx - 24, cy - 24, 48, "doc", C.milk, C.esp, `border:1px solid ${C.sand}`) + T(cx - 40, cy + 30, "notes.txt", `color:${C.cocoa}`);
  h += B(cx - R - 42, cy - R - 42, 2 * R + 84, 2 * R + 84, `border-radius:50%;background:rgba(255,253,248,.45);border:1px solid rgba(255,255,255,.85)`);
  ["Open", "Share", "Move", "Rename", "Trash", "More"].forEach((a, i) => {
    const ang = (-90 + i * 60) * Math.PI / 180, x = cx + R * Math.cos(ang), y = cy + R * Math.sin(ang), on = i === 1;
    h += shadow(x - 34, y - 20, 68, 40, 20) + B(x - 34, y - 20, 68, 40, `background:${on ? C.straw : C.milk};border:1px solid ${on ? C.straw : C.sand};border-radius:20px`) + TC(x - 34, y - 8, 68, a, `color:${on ? C.milk : C.esp}`);
  });
  h += T(660, 470, "Press and hold, flick toward an action.", `color:${C.cocoa}`) + T(660, 490, "Works with mouse, touch or a gamepad stick.", `color:${C.cocoa}`) + T(660, 510, "Keys 1-6 pick the same slots.", `color:${C.cocoa}`);
  // the dial
  const modes = [["Study", C.honey], ["Build", C.blue], ["Play", C.straw], ["Rest", C.matcha], ["Out", C.lav]];
  const dx = 230, dy = 690;
  h += B(dx - 180, dy - 52, 360, 104, `background:rgba(255,253,248,.7);border:1px solid rgba(255,255,255,.9);border-radius:52px`);
  modes.forEach(([n, c], i) => { const x = dx - 150 + i * 72, on = i === 1; h += B(x - (on ? 30 : 22), dy - (on ? 30 : 22), on ? 60 : 44, on ? 60 : 44, `background:${on ? c : c + "88"};border-radius:50%;border:${on ? 4 : 0}px solid ${C.milk}`) + TC(x - 36, dy + 34, 72, n, on ? `${BD}color:${C.esp}` : `color:${C.cocoa}`); });
  h += T(440, 664, "Mode dial: Ctrl-Tab, or swipe along the bottom.", `color:${C.cocoa}`) + T(440, 684, "A mode sets: which apps lead, what notifications", `color:${C.cocoa}`) + T(440, 702, "get through, the accent colour and the wallpaper.", `color:${C.cocoa}`);
  return h;
}

const SLIDES = [
  { id: "rewind", n: "A", name: "Rewind", tag: "The whole computer has a timeline", draw: rewind, cost: "hard",
    idea: "There is no \"save\" or \"undo\" anxiety: the OS keeps a timeline of your work (which windows were open, which files existed, what they said). Scrub back to 9:12 this morning, see the screen as it was, and either restore that moment or drag one window or file out of it.",
    new_: "Time Machine meets a journaling filesystem, but front and centre as the main way to navigate, not a backup app.",
    why: "The real cost is underneath: copy-on-write or journaled files, plus an event log of window layouts. The first version needs no pixels at all: store file versions and window lists, and draw the past as simple window shapes. Capturing screenshots of old moments can come much later.",
    els: ["Timeline scrubber", "Moments list (auto-named)", "Past-state viewer", "Restore / pull-out actions", "File version store", "Window layout log", "Search your past"],
    font: "Unifont is fine. The activity colours and the playhead carry most of the look, so this one is mostly rectangles." },
  { id: "canvas", n: "B", name: "Canvas", tag: "One endless surface, zoom instead of switching windows", draw: canvas, cost: "hard",
    idea: "No desktops, no window switcher. Everything lives on one big surface. Zoom out and windows collapse into named cards grouped by project; zoom in and a card becomes a real window. Related things sit near each other with a line between them.",
    new_: "Semantic zoom: what you see changes meaning as you zoom, so \"where is my school work\" is a place, not a search.",
    why: "Needs a world-to-screen transform, hit testing in world coordinates, and scaled drawing of windows. To keep it kernel-friendly, use three fixed zoom levels (far: labels only, mid: cards, near: real windows) so text is only ever drawn at 1x, 2x or 4x and never shrunk.",
    els: ["Infinite surface + dot grid", "Cluster labels", "Cards that become windows", "Connector lines", "Zoom control", "Minimap", "Pan by drag or keys"],
    font: "Never shrink the Unifont bitmap. Far-zoom labels are drawn at 2x or 4x, which is on-brand and crisp. This is the concept that suits a bitmap font best." },
  { id: "scroll", n: "C", name: "Scroll", tag: "A notebook shell where every command leaves a live card", draw: scroll, cost: "medium",
    idea: "The terminal's output is not text, it is a card. ls shows thumbnails, disk shows bars, edit opens an editor inline, play shows a player. Cards stay in the notebook, can be re-run, and can be pinned to a rail where they keep updating.",
    new_: "A shell and a dashboard in one. Linux soul (everything is a command) with a UI that is allowed to show pictures, bars and controls.",
    why: "No window manager needed, which makes it the cheapest of the four. You need a cell list, a handful of output renderers (text, table, image grid, bar, player) and a pin rail. Start with text-only cells and add renderers one at a time.",
    els: ["Notebook of cells", "Rich output cards", "Prompt with completions", "Pinned live rail", "Re-run / reorder cells", "Command palette"],
    font: "Unifont is a natural fit. A bold face would help the cell titles; the rest works as is." },
  { id: "dial", n: "D", name: "Dial", tag: "Modes reshape the OS; radial menus for everything", draw: dial, cost: "medium",
    idea: "You tell the OS what you are doing (Study, Build, Play, Rest, Out) and it rearranges: which apps lead, which notifications get through, the accent colour, the wallpaper. Context actions are radial: press and hold on anything and flick toward an action.",
    new_: "Purpose first instead of app first, plus one menu style that works with mouse, touch, gamepad or number keys.",
    why: "A mode is just a named record (app list, notification filter, theme), so the logic is small. Radial menus need a circle drawn with rounded rects and an angle lookup (atan2 or a small table) for flick direction. Touch is optional.",
    els: ["Mode dial", "Mode home cards", "Held-notifications queue", "Radial context menu", "Per-mode themes", "Number-key shortcuts"],
    font: "The big mode name and the radial labels want a nicer font. Unifont at 3x for the title is acceptable but blocky." },
];

/* ------------------------------ page ------------------------------ */
const N = SLIDES.length;
const id = (i) => `n${((i % N) + N) % N}`;
const svg = (s) => `<svg class="scene" viewBox="0 0 1024 768" role="img" aria-label="Mockup of ${s.name}"><foreignObject x="0" y="0" width="1024" height="768"><div class="stage">${s.draw()}</div></foreignObject></svg>`;
const rules = SLIDES.map((s, i) => `#r-${id(i)}:checked ~ .wrap .p-${id(i)}{display:grid}\n#r-${id(i)}:checked ~ .wrap .n-${id(i)}{display:flex}\n#r-${id(i)}:checked ~ .wrap .t-${id(i)}{border-color:var(--accent)}`).join("\n");

const html = `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>milkOS: 4 new concepts</title>
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
<header><div><h1>milkOS: 4 new concepts</h1><div class="sub">Not new looks, new ideas about what an OS screen is for. Each is still drawable by the kernel with rectangles, rounded rects, text on the 8×16 grid, circles and alpha. Use Prev / Next or tap a thumbnail.</div></div>
${SLIDES.map((s, i) => `<div class="nav n-${id(i)}"><label class="btn" for="r-${id(i - 1)}" role="button" tabindex="0" aria-label="Previous">&larr; Prev</label><span class="count">${i + 1} / ${N}</span><label class="btn" for="r-${id(i + 1)}" role="button" tabindex="0" aria-label="Next">Next &rarr;</label></div>`).join("")}
</header>
${SLIDES.map((s, i) => `<article class="panel p-${id(i)}"><div class="stagebox"><div class="stagewrap">${svg(s)}</div><div class="cap">${s.n} ${s.name}: drawn 1:1 at 1024×768 and scaled to fit.</div></div>
<aside class="notes"><h2>${s.n}. ${s.name}</h2><p class="tag">${s.tag}</p><span class="cost ${s.cost}">${s.cost}</span>
<h3>Key idea</h3><p>${s.idea}</p><h3>What is new about it</h3><p>${s.new_}</p><h3>Cost in a kernel-drawn UI</h3><p>${s.why}</p>
<h3>Main elements</h3><ul class="chips">${s.els.map((e) => `<li>${e}</li>`).join("")}</ul><h3>Font note</h3><p>${s.font}</p></aside></article>`).join("")}
<div class="strip">${SLIDES.map((s, i) => `<label class="thumb t-${id(i)}" for="r-${id(i)}" tabindex="0" role="button" aria-label="Show ${s.name}"><div class="tw">${svg(s)}</div><div class="tl"><b>${s.n}. ${s.name}</b><span>${s.cost}</span></div></label>`).join("")}</div>
<section class="plan"><h2>Design notes</h2>
<p><b>Cheapest to try first: C Scroll.</b> It needs no window manager, it is very Linux, and the rich cards are small renderers you can add one at a time. It could even live as the terminal app inside the 1.0 desktop.</p>
<p style="margin-top:8px"><b>Most distinctive: A Rewind.</b> Nothing else has "the whole computer has a timeline" as its front door. Start with file versions and window lists only, and treat screenshots of the past as a later upgrade.</p>
<p style="margin-top:8px"><b>B Canvas</b> suits a bitmap font surprisingly well (labels at 2x and 4x, never shrunk). <b>D Dial</b> is the lightest to build but works best layered on top of another look (the modes and radial menus can sit on the 1.0 desktop).</p></section>
</div>
<script>addEventListener('keydown',function(e){var r=[].slice.call(document.querySelectorAll('.r')),c=r.findIndex(function(x){return x.checked});if(e.key==='ArrowRight')r[(c+1)%r.length].checked=true;else if(e.key==='ArrowLeft')r[(c-1+r.length)%r.length].checked=true;else if((e.key==='Enter'||e.key===' ')&&e.target.tagName==='LABEL'){e.preventDefault();e.target.click()}})</script>
</body></html>`;

const here = dirname(fileURLToPath(import.meta.url));
writeFileSync(join(here, "new-concepts.html"), html);
console.log("wrote new-concepts.html", html.length, "bytes");
