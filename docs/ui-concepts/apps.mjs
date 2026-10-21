// Specs and mockups for five built-in apps: Activity Monitor, Text Editor, Calendar, Clock/Timer, Preview.
// Run: node docs/ui-concepts/apps.mjs. Writes apps.html (mockups) and SPEC-APPS.md (the spec, numbers computed from the tokens).
import { B, T, TC, TR, TRi, icon, drop, C, X2, X3, BD } from "./scenes.mjs";
import { TOK, DARK, WP, glass, shadow as shadowLayers, menubar, dock, lightsX, dim } from "./v1.mjs";
import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const shadow = (x, y, w, h, r, layers = TOK.shadow.window) => shadowLayers(x, y, w, h, r, layers);
const INK = "#3A2E27";
const circle = (x, y, d, bg, extra = "") => B(x, y, d, d, `background:${bg};border-radius:50%;${extra}`);
const lin = (v) => { v /= 255; return v <= 0.04045 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4); };
const lumOf = (h) => { const [r, g, b] = [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16)); return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b); };
const ratio = (a, b) => { const [x, y] = [lumOf(a), lumOf(b)].sort((p, q) => q - p); return +((x + 0.05) / (y + 0.05)).toFixed(2); };
const mixHex = (a, b, t) => "#" + [1, 3, 5].map((i) => Math.round(parseInt(a.slice(i, i + 2), 16) * (1 - t) + parseInt(b.slice(i, i + 2), 16) * t).toString(16).padStart(2, "0")).join("");
const L = Object.fromEntries(Object.entries(TOK.color).map(([k, v]) => [k, v[0]]));

/* buttons per SPEC Buttons */
const pill = (x, y, w, txt, kind = "primary") => B(x, y, w, 24, `background:${kind === "primary" ? C.straw : kind === "danger" ? "#B5473B" : C.oat};border-radius:12px;${kind === "secondary" ? `border:1px solid ${C.sand}` : ""}`) + TC(x, y + 4, w, `<b>${txt}</b>`, `color:${kind === "danger" ? "#FFFDF8" : INK}`);
const btn = (x, y, w, txt, kind = "primary") => B(x, y, w, 28, `background:${kind === "primary" ? C.straw : kind === "danger" ? "#B5473B" : C.oat};border-radius:8px;${kind === "secondary" ? `border:1px solid ${C.sand}` : ""}`) + TC(x, y + 6, w, `<b>${txt}</b>`, `color:${kind === "danger" ? "#FFFDF8" : INK}`);
const chip = (x, y, w, txt, on = false) => B(x, y, w, 24, `background:${on ? C.esp : "rgba(255,253,248,.85)"};border:1px solid ${on ? C.esp : C.sand};border-radius:12px`) + TC(x, y + 4, w, txt, `color:${on ? C.cream : C.esp}`);
const seg = (x, y, labels, activeIdx, w = 84) => B(x, y, labels.length * w + 4, 28, `background:${C.oat};border:1px solid ${C.sand};border-radius:14px`) + labels.map((l, i) => (i === activeIdx ? B(x + 2 + i * w, y + 2, w, 24, `background:${C.milk};border:1px solid ${C.sand};border-radius:12px`) : "") + TC(x + 2 + i * w, y + 6, w, i === activeIdx ? `<b>${l}</b>` : l, i === activeIdx ? "" : `color:#6B5846`)).join("");
const searchBox = (x, y, w, ph) => B(x, y, w, 24, `background:${C.milk};border:1px solid ${C.sand};border-radius:12px`) + T(x + 10, y + 4, ph, `color:#6B5846`);
const appWin = (x, y, w, hh, title, body, dark = false) => shadow(x, y, w, hh, 12) + B(x, y, w, hh, `background:${C.milk};border:1px solid ${C.sand};border-radius:12px;overflow:hidden`, B(0, 0, w, 32, `background:${C.cream};border-bottom:1px solid ${C.sand}`) + lightsX(13, 10) + TC(0, 8, w, `<b>${title}</b>`) + body);
const base = (menuName) => WP + menubar(menuName);
const CH = { blue: "#4B63A8", honey: "#A87812", matcha: "#5B8A5E", straw: "#C9566B" };
const gauge = (x, y, w, h, v, c) => B(x, y, w, h, `background:${C.sand};border-radius:${h / 2}px`, B(0, 0, w * v, h, `background:${c};border-radius:${h / 2}px`));
// history chart from columns
const chart = (x, y, w, h, series, c, bg = true) => B(x, y, w, h, `background:${bg ? C.cream : "transparent"};border:1px solid ${C.sand};border-radius:8px;overflow:hidden`, series.map((v, i) => B(i * (w / series.length), h - Math.max(1, v * (h - 2)), Math.max(2, w / series.length - 1), Math.max(1, v * (h - 2)), `background:${c}`)).join(""));
const wave = (n, f, ph, amp = 0.4, off = 0.35) => Array.from({ length: n }, (_, i) => Math.max(0.03, Math.min(0.98, off + amp * Math.sin(i / f + ph) * (0.6 + 0.4 * Math.sin(i / (f * 2.7))))));

/* ================= ACTIVITY MONITOR ================= */
const PROCS = [
  ["wm", "gear", C.latte, 4, "running", 6.2, 38.4, 5, "System"],
  ["Terminal", "term", C.esp, 15, "running", 1.8, 9.6, 2, ""],
  ["msh", "term", C.esp, 11, "sleeping", 0.0, 1.1, 1, ""],
  ["Files", "folder", C.honey, 14, "sleeping", 0.4, 12.2, 2, ""],
  ["drop", "gear", C.latte, 7, "sleeping", 0.1, 0.8, 1, "System"],
  ["audit", "gear", C.latte, 6, "sleeping", 0.1, 2.4, 1, "System"],
  ["moo2048", "term", C.cream, 21, "running", 2.7, 5.9, 1, "Sandboxed"],
  ["peek", "doc", C.cream, 22, "sleeping", 0.0, 2.2, 1, "Sandboxed"],
  ["httpd", "globe", C.blue, 9, "sleeping", 0.0, 1.4, 2, ""],
  ["init", "gear", C.latte, 1, "sleeping", 0.0, 0.4, 1, "System"],
];
function amProcesses() {
  let h = base("Activity Monitor") + dock([0, 1]);
  const x = 60, y = 52, w = 800, hh = 640;
  let b = B(0, 32, w, 44, `background:${C.cream};border-bottom:1px solid ${C.sand}`) + seg(12, 40, ["Processes", "CPU", "Memory", "Disk", "Security"], 0, 76) + searchBox(w - 230, 44, 140, "Search") + pill(w - 78, 44, 66, "Quit", "danger");
  const cols = [["Name", 16], ["PID", 306], ["State", 366], ["CPU %", 466], ["Memory", 556], ["Threads", 648], ["Sandbox", 716]];
  b += B(0, 76, w, 28, `background:${C.oat};border-bottom:1px solid ${C.sand}`) + cols.map(([n, cx]) => T(cx, 82, n, `color:#6B5846`)).join("") + T(466 + 40, 82, "&#9660;", `color:#6B5846`);
  PROCS.forEach((p, i) => {
    const ry = 104 + i * 28, sel = i === 6;
    b += (sel ? B(0, ry, w, 28, `background:${C.straw}33`) : (i % 2 ? B(0, ry, w, 28, "background:rgba(239,230,212,.35)") : "")) + icon(14, ry + 4, 20, p[1], p[2], p[1] === "term" && p[2] === C.esp ? C.cream : C.esp, p[2] === C.cream ? `border:1px solid ${C.sand}` : "") + T(42, ry + 6, p[0]) + T(306, ry + 6, String(p[3])) + T(366, ry + 6, p[4]) + TRi(w, 506, ry + 6, p[5].toFixed(1)) + TRi(w, 620, ry + 6, p[6].toFixed(1) + " MB") + T(668, ry + 6, String(p[7])) + (p[8] ? B(716, ry + 4, 72, 20, `background:${p[8] === "Sandboxed" ? C.matcha + "55" : C.oat};border-radius:10px`) + TC(716, ry + 6, 72, p[8], `color:${INK};font-size:12px;line-height:16px`) : "");
  });
  // detail panel for selected row
  const dy = 104 + 10 * 28 + 8;
  b += B(12, dy, w - 24, 150, `background:${C.cream};border:1px solid ${C.sand};border-radius:12px`, T(16, 12, "<b>moo2048</b>   pid 21   started 10:32   parent msh (11)") + T(16, 36, "/Applications/moo2048", `color:#6B5846`) +
    T(16, 62, "Can read", `color:#6B5846`) + T(110, 62, "its own folder, 2 files you chose") + T(16, 82, "Network", `color:#6B5846`) + T(110, 82, "none") + T(16, 102, "Refused", `color:#6B5846`) + T(110, 102, "1 time today (/etc/passwd)", `color:#B5473B`) + T(16, 126, "Open files 4   Threads 1", `color:#6B5846`) + pill(w - 24 - 170, 10, 74, "Show Files", "secondary") + pill(w - 24 - 90, 10, 78, "Security", "secondary"));
  // summary bar
  b += B(0, hh - 28, w, 26, `background:${C.cream};border-top:1px solid ${C.sand}`) + T(16, hh - 24, "10 processes   14 threads   Memory 124 MB of 512 MB   CPU 11%", `color:#6B5846`) + TRi(w, w - 16, hh - 24, "updates every 1 s", `color:#6B5846`);
  return h + appWin(x, y, w, hh, "Activity Monitor", b);
}
function amCpu() {
  let h = base("Activity Monitor") + dock([1]);
  const x = 60, y = 52, w = 800, hh = 640;
  let b = B(0, 32, w, 44, `background:${C.cream};border-bottom:1px solid ${C.sand}`) + seg(12, 40, ["Processes", "CPU", "Memory", "Disk", "Security"], 1, 76);
  b += T(20, 92, "CPU, last 60 seconds", `color:#6B5846`);
  [0, 1, 2, 3].forEach((c, i) => { const cx = 20 + (i % 2) * 388, cy = 118 + Math.floor(i / 2) * 150; b += B(cx, cy, 372, 138, `background:${C.cream};border:1px solid ${C.sand};border-radius:12px`, T(12, 8, `<b>CPU ${c}</b>`) + TRi(372, 360, 8, [14, 9, 22, 4][c] + "%") + chart(12, 32, 348, 90, wave(60, 5 + i, i * 1.7, 0.25, [0.16, 0.12, 0.26, 0.08][i]), CH.blue)); });
  b += B(20, 424, 760, 190, `background:${C.cream};border:1px solid ${C.sand};border-radius:12px`, T(14, 10, "<b>Load</b>") + T(14, 36, "1 min 0.42    5 min 0.35    15 min 0.31", `color:#6B5846`) + T(14, 70, "Busiest now", `color:#6B5846`) +
    ["wm  6.2%", "moo2048  2.7%", "Terminal  1.8%"].map((t, i) => T(14, 94 + i * 24, t)).join("") + T(380, 10, "<b>Interrupts</b>") + T(380, 36, "timer 100/s   net 12/s   input 3/s   sound 0/s", `color:#6B5846`));
  return h + appWin(x, y, w, hh, "Activity Monitor", b);
}
function amMemory() {
  let h = base("Activity Monitor") + dock([1]);
  const x = 60, y = 52, w = 800, hh = 640;
  let b = B(0, 32, w, 44, `background:${C.cream};border-bottom:1px solid ${C.sand}`) + seg(12, 40, ["Processes", "CPU", "Memory", "Disk", "Security"], 2, 76);
  b += T(20, 92, "Memory", `color:#6B5846`) + T(20, 112, "124 MB used of 512 MB", X2.replace("26.667px", "16px").replace("32px", "20px") + BD);
  b += B(20, 142, 760, 28, `background:${C.oat};border:1px solid ${C.sand};border-radius:14px;overflow:hidden`, B(0, 0, 760 * 0.15, 28, `background:${C.honey}`) + B(760 * 0.15, 0, 760 * 0.09, 28, `background:${C.blue}`) + B(760 * 0.24, 0, 760 * 0.03, 28, `background:${C.lav}`));
  [[C.honey, "Apps", "76 MB"], [C.blue, "Kernel heap", "46 MB"], [C.lav, "Page cache", "14 MB"], [C.oat, "Free", "388 MB"]].forEach((k, i) => { b += circle(20 + i * 190, 182, 12, k[0], `border:1px solid ${C.sand}`) + T(40 + i * 190, 180, `${k[1]}  ${k[2]}`); });
  b += T(20, 222, "Memory pressure", `color:#6B5846`) + gauge(150, 224, 180, 12, 0.18, C.matcha) + T(344, 222, "Low");
  b += T(20, 262, "Biggest users", `color:#6B5846`);
  PROCS.slice().sort((a, c) => c[6] - a[6]).slice(0, 6).forEach((p, i) => { b += T(20, 290 + i * 34, p[0]) + gauge(180, 292 + i * 34, 400, 14, p[6] / 40, C.honey) + TRi(w, w - 20, 290 + i * 34, p[6].toFixed(1) + " MB"); });
  b += T(20, 510, "Source: /proc/meminfo and each process's VmRSS. Page cache is memory the system can give back.", `color:#6B5846`);
  return h + appWin(x, y, w, hh, "Activity Monitor", b);
}
const SEC_EVENTS = [
  ["10:41", "peek was refused", "tried to read /etc/passwd", "#B5473B", "sandbox.deny"],
  ["10:40", "You gave peek a file", "~/Documents/todo.txt through the Open panel", C.matcha, "powerbox.grant"],
  ["10:32", "moo2048 started", "sandboxed: own folder only, no network", C.matcha, "sandbox.enter"],
  ["10:12", "fortune was installed", "package from the Milk Store", C.blue, "user.pkg.install"],
  ["10:05", "Milk Drop received bike.png", "from alice, checked OK", C.matcha, "user.drop.recv"],
  ["09:58", "bob's key was trusted", "first use of a new device", C.honey, "user.drop.trust"],
  ["09:40", "Terminal started a program", "/bin/ls", C.latte, "exec"],
];
function amSecurity() {
  let h = base("Activity Monitor") + dock([1]);
  const x = 60, y = 52, w = 800, hh = 640;
  let b = B(0, 32, w, 44, `background:${C.cream};border-bottom:1px solid ${C.sand}`) + seg(12, 40, ["Processes", "CPU", "Memory", "Disk", "Security"], 4, 76) + searchBox(w - 190, 44, 170, "Search events");
  b += B(20, 88, 760, 48, `background:${C.matcha}22;border:1px solid ${C.matcha};border-radius:12px`, T(16, 8, "<b>&#10003; Log is intact</b>") + T(16, 26, "4,213 records, chain verified 10:41", `color:#6B5846`) + pill(760 - 140, 12, 124, "Verify again", "secondary"));
  ["All", "Refused", "Files", "Programs", "Installs", "Milk Drop"].forEach((c, i) => { b += chip(20 + i * 96, 148, 88, c, i === 0); });
  SEC_EVENTS.forEach((e, i) => { const ey = 190 + i * 58; b += circle(28, ey + 6, 12, e[3]) + (i < SEC_EVENTS.length - 1 ? B(33, ey + 20, 2, 44, `background:${C.sand}`) : "") + T(54, ey, `<b>${e[1]}</b>`) + T(54, ey + 20, e[2], `color:#6B5846`) + TRi(w, w - 20, ey, e[0], `color:#6B5846`) + TRi(w, w - 20, ey + 20, e[4], `color:#6B5846`); });
  b += T(20, hh - 40, "Read-only. Source: /proc/audit. Every event type is shown as a sentence; the raw record is one click away.", `color:#6B5846`);
  return h + appWin(x, y, w, hh, "Activity Monitor", b);
}

/* ================= TEXT EDITOR ================= */
const SYN_L = { keyword: "#3D5A99", string: "#42704B", number: "#8A5A00", comment: "#6B5846", error: "#B5473B", text: L.espresso, bg: L.milk };
const SYN_D = { keyword: "#A9BBEA", string: "#A8CFA6", number: "#EBB650", comment: "#BEA890", error: "#F0A0A0", text: DARK.espresso, bg: DARK.milk };
const CODE = [
  [["kw", "use "], ["t", "core::fmt::Write;"]], [], [["kw", "mod "], ["t", "mem;"]], [["kw", "mod "], ["t", "display;"]], [], [["cm", "// the first thing the kernel does"]],
  [["kw", "pub extern "], ["st", "\"C\" "], ["kw", "fn "], ["t", "kmain() -> ! {"]], [["t", "    uart::init();"]], [["t", "    mem::init(dtb);"]], [["t", "    println!("], ["st", "\"milkOS up\""], ["t", ");"]], [["t", "    "], ["kw", "let "], ["t", "pages = "], ["nm", "512"], ["t", ";"]], [["t", "    "], ["kw", "loop "], ["t", "{ wfi(); }"]], [["t", "}"]],
];
const synHtml = (line, S) => line.map(([k, s]) => `<span style="color:${{ kw: S.keyword, st: S.string, nm: S.number, cm: S.comment, t: S.text }[k]}">${s.replace(/</g, "&lt;")}</span>`).join("");
function editorBody(x0, y0, w, hh, { findBar = false, tabs = false, dark = false, active = 8, matchLines = [] } = {}) {
  const S = dark ? SYN_D : SYN_L, gut = dark ? DARK.cream : L.cream, bg = dark ? DARK.milk : L.milk, ln = dark ? DARK.cocoa : L.cocoa, hl = dark ? "#382D27" : "#F6EFE0";
  let b = "";
  const top = tabs ? 38 : 32;
  if (findBar) b += B(0, top, w, 40, `background:${gut};border-bottom:1px solid ${dark ? DARK.sand : L.sand}`) + B(12, top + 8, 150, 24, `background:${bg};border:1px solid ${dark ? DARK.sand : L.sand};border-radius:12px`, T(10, 4, "kmain", `color:${S.text}`)) + T(172, top + 12, "2 of 3", `color:${ln}`) + pill(226, top + 8, 28, "&lt;", "secondary") + pill(258, top + 8, 28, "&gt;", "secondary") + B(300, top + 8, 36, 24, `background:${C.oat};border:1px solid ${C.sand};border-radius:12px`) + TC(300, top + 12, 36, "Aa") + B(348, top + 8, 150, 24, `background:${bg};border:1px solid ${dark ? DARK.sand : L.sand};border-radius:12px`, T(10, 4, "start", `color:${ln}`)) + pill(506, top + 8, 70, "Replace", "secondary") + pill(582, top + 8, 40, "All", "secondary");
  const ty = top + (findBar ? 40 : 0), th = hh - ty - 24;
  b += B(0, ty, 48, th, `background:${gut};border-right:1px solid ${dark ? DARK.sand : L.sand}`) + B(48, ty, w - 48, th, `background:${bg}`);
  CODE.forEach((ln_, i) => {
    const y = ty + 10 + i * 20, cur = i === active;
    b += (cur ? B(49, y - 1, w - 49, 20, `background:${hl}`) : "") + TRi(w, 38 + 0, y, String(i + 1 + 8), `color:${ln}`).replace(`right:${w - 38}px`, `right:${w - 40}px`).replace("</div>", "</div>") + T(60, y, synHtml(ln_, S));
    if (matchLines.includes(i)) b += B(60 + 8 * (i === 6 ? 13 : i === 9 ? 0 : 4), y - 1, 5 * 8 + 4, 18, `background:${i === 6 ? C.straw + "66" : C.honey + "66"};border-radius:3px`);
  });
  b += B(60 + 4 * 8, ty + 10 + active * 20, 2, 18, `background:${S.text}`);
  b += B(0, hh - 24, w, 24, `background:${gut};border-top:1px solid ${dark ? DARK.sand : L.sand}`) + T(14, hh - 20, "Ln 17, Col 5    UTF-8    LF    Spaces: 4    Rust", `color:${ln}`) + TRi(w, w - 14, hh - 20, "Saved", `color:${ln}`);
  return b;
}
function teEdit(dark = false, find = false) {
  let h = base("Text Editor") + dock([0, 3]);
  const x = 60, y = 56, w = 700, hh = 540;
  return h + appWin(x, y, w, hh, "main.rs", editorBody(x, y, w, hh, { findBar: find, matchLines: find ? [6, 7, 9] : [] }));
}
function teTabs() {
  let h = base("Text Editor") + dock([0, 3]);
  const x = 60, y = 56, w = 700, hh = 540;
  let b = B(0, 0, w, 38, `background:${L.cream};border-bottom:1px solid ${L.sand}`) + lightsX(13, 12);
  [["notes.md", 1], ["main.rs", 0], ["todo.txt", 0]].forEach((t, i) => { const tx = 84 + i * 150; b += B(tx, 6, 144, 32, `background:${t[1] ? L.milk : "transparent"};border:${t[1] ? 1 : 0}px solid ${L.sand};border-bottom:0;border-radius:10px 10px 0 0`) + T(tx + 12, 11, t[1] ? `<b>${t[0]}</b>${i === 0 ? " &#9679;" : ""}` : t[0], t[1] ? "" : `color:#6B5846`) + TRi(w, tx + 136, 11, "x", `color:#6B5846`); });
  b += B(84 + 3 * 150 + 4, 10, 28, 24, `background:${C.oat};border-radius:8px`) + TC(84 + 3 * 150 + 4, 14, 28, "+");
  b += B(0, 38, 48, hh - 38 - 24, `background:${L.cream};border-right:1px solid ${L.sand}`);
  const md = [["# milkOS notes", "h"], ["", ""], ["## Ideas", "h"], ["- ship the compositor", "li"], ["- pick a nicer font", "li"], ["- a boot logo, a milk drop", "li"], ["", ""], ["Remember: **autosave is off**,", "p"], ["press Cmd-S. The dot in the tab", "p"], ["means there are unsaved changes.", "p"], ["", ""], ["`cargo build` runs in the Terminal.", "p"]];
  md.forEach((m, i) => { const yy = 38 + 10 + i * 20; b += TRi(w, w - 662, yy, String(i + 1), `color:#6B5846`) + T(60, yy, m[1] === "h" ? `<b>${m[0]}</b>` : m[1] === "li" ? `<span style="color:${C.straw === "x" ? "" : "#B5465B"}">-</span>${m[0].slice(1)}` : m[0].replace(/`([^`]+)`/g, '<span style="background:#EFE6D4">$1</span>').replace(/\*\*([^*]+)\*\*/g, "<b>$1</b>")); });
  b += B(60 + 6 * 8, 38 + 10 + 7 * 20 + 18, 160, 2, `background:${C.straw}`);
  b += B(0, hh - 24, w, 24, `background:${L.cream};border-top:1px solid ${L.sand}`) + T(14, hh - 20, "Ln 8, Col 7    UTF-8    LF    Soft wrap on    Markdown", `color:#6B5846`) + TRi(w, w - 14, hh - 20, "Edited", `color:#6B5846`);
  return h + appWin(x, y, w, hh, "", b).replace("<b></b>", "");
}

/* ================= CALENDAR ================= */
const CALS = [["Personal", C.straw], ["Home", C.matcha], ["Work", C.blue]];
function calSidebar(sw, hh) {
  let b = B(0, 32, sw, hh - 34, `background:${L.oat};border-right:1px solid ${L.sand}`) + T(14, 46, "<b>October 2026</b>");
  ["M", "T", "W", "T", "F", "S", "S"].forEach((d, i) => { b += TC(10 + i * 26, 70, 26, d, `color:#6B5846`); });
  for (let i = 0; i < 35; i++) { const n = i - 3 + 1, col = i % 7, row = Math.floor(i / 7), today = n === 21, cx = 10 + col * 26, cy = 90 + row * 22; b += (today ? circle(cx + 1, cy - 2, 24, C.straw) : "") + TC(cx, cy, 26, n < 1 ? String(30 + n) : n > 31 ? String(n - 31) : String(n), today ? `color:${INK};${BD}` : n < 1 || n > 31 ? `color:${L.latte}` : ""); }
  b += T(14, 222, "CALENDARS", `color:#6B5846`);
  CALS.forEach((c, i) => { b += circle(16, 250 + i * 28, 14, c[1]) + T(38, 248 + i * 28, c[0]); });
  b += T(14, hh - 90, "Next: Dentist 2:30 PM", `color:#6B5846`) + pill(14, hh - 62, 100, "New event");
  return b;
}
function calMonth() {
  let h = base("Calendar") + dock([2, 4]);
  const x = 40, y = 48, w = 940, hh = 676, sw = 172, gx = sw + 8, gw = w - sw - 16, cw = gw / 7;
  let b = calSidebar(sw, hh) + B(sw, 32, w - sw, 40, `background:${L.cream};border-bottom:1px solid ${L.sand}`) + seg(sw + 12, 38, ["Day", "Week", "Month", "Year"], 2, 64) + T(sw + 320, 46, "<b>October 2026</b>", X2.replace("26.667px", "16px").replace("32px", "20px")) + pill(w - 210, 42, 60, "Today", "secondary") + pill(w - 140, 42, 28, "&lt;", "secondary") + pill(w - 108, 42, 28, "&gt;", "secondary") + searchBox(w - 316, 42, 100, "Search");
  ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"].forEach((d, i) => { b += TC(gx + i * cw, 78, cw, d, `color:#6B5846`); });
  const ev = { 5: [["Bike ride", C.matcha]], 8: [["Dentist 2:30", C.straw]], 12: [["Team sync", C.blue], ["Lunch", C.matcha]], 14: [["Call Mum", C.straw]], 18: [["Pick up bike", C.matcha], ["Study", C.blue], ["Gym", C.matcha], ["+2 more", null]], 21: [["Dentist 2:30", C.straw], ["Dinner 7:30", C.matcha]], 25: [["Release", C.blue]] };
  for (let i = 0; i < 35; i++) {
    const n = i - 3 + 1, col = i % 7, row = Math.floor(i / 7), cx = gx + col * cw, cy = 100 + row * 112, today = n === 21, out = n < 1 || n > 31;
    b += B(cx, cy, cw, 112, `border:1px solid ${L.sand};background:${out ? "rgba(239,230,212,.45)" : "transparent"}`) + (today ? circle(cx + 6, cy + 4, 24, C.straw) : "") + T(cx + 12, cy + 8, String(n < 1 ? 30 + n : n > 31 ? n - 31 : n), today ? `color:${INK};${BD}` : out ? `color:${L.latte}` : "");
    (ev[n] || []).slice(0, 4).forEach((e, k) => { b += e[1] ? B(cx + 4, cy + 34 + k * 18, cw - 8, 17, `background:${e[1]}44;border-radius:4px`, B(0, 0, 3, 17, `background:${e[1]}`) + T(8, 0, e[0], `color:${L.espresso};font-size:12px;line-height:17px`)) : T(cx + 8, cy + 34 + k * 18, e[0], `color:#6B5846;font-size:12px;line-height:17px`); });
  }
  return h + appWin(x, y, w, hh, "Calendar", b);
}
function calWeek() {
  let h = base("Calendar") + dock([2, 4]);
  const x = 40, y = 48, w = 940, hh = 676, sw = 172, gx = sw + 56, gw = w - sw - 64, cw = gw / 7;
  let b = calSidebar(sw, hh) + B(sw, 32, w - sw, 40, `background:${L.cream};border-bottom:1px solid ${L.sand}`) + seg(sw + 12, 38, ["Day", "Week", "Month", "Year"], 1, 64) + T(sw + 320, 46, "<b>19 to 25 October</b>", X2.replace("26.667px", "16px").replace("32px", "20px"));
  ["Mon 19", "Tue 20", "Wed 21", "Thu 22", "Fri 23", "Sat 24", "Sun 25"].forEach((d, i) => { b += TC(gx + i * cw, 80, cw, i === 2 ? `<b>${d}</b>` : d, i === 2 ? "" : `color:#6B5846`) + (i === 2 ? B(gx + i * cw + cw / 2 - 22, 98, 44, 3, `background:${C.straw};border-radius:2px`) : ""); });
  for (let hr = 8; hr <= 18; hr++) { const yy = 112 + (hr - 8) * 48; b += B(gx - 6, yy, gw + 6, 1, `background:${L.sand}`) + TRi(w, w - (w - (gx - 10)), yy - 8, `${hr}:00`, `color:#6B5846`); }
  b += B(gx - 6, 112 + (10.7 - 8) * 48, gw + 6, 2, `background:${C.straw}`) + circle(gx - 12, 112 + (10.7 - 8) * 48 - 5, 10, C.straw);
  const evs = [[0, 9, 10.5, "Study", C.blue], [1, 13, 14, "Lunch", C.matcha], [2, 14.5, 15.5, "Dentist", C.straw], [2, 19 - 1, 18.9, "", null], [3, 10, 12, "Team sync", C.blue], [4, 9, 9.5, "Gym", C.matcha]];
  evs.forEach(([d, s, e, t, c]) => { if (!c) return; const ex = gx + d * cw + 3, ey = 112 + (s - 8) * 48, eh = (e - s) * 48; b += B(ex, ey, cw - 6, eh, `background:${c}44;border-radius:6px;border:${d === 2 && t === "Dentist" ? 2 : 0}px solid ${C.straw}`, B(0, 0, 3, eh, `background:${c}`) + T(8, 4, t, `color:${L.espresso}`)); });
  // event sheet
  const sx = gx + 3 * cw - 30, sy = 190, sw2 = 400, sh = 400;
  const sheet = shadow(sx, sy, sw2, sh, 14) + B(sx, sy, sw2, sh, `background:${L.milk};border:1px solid ${L.sand};border-radius:14px;overflow:hidden`, B(0, 0, sw2, 44, `background:${L.cream};border-bottom:1px solid ${L.sand}`) + T(16, 14, "<b>New event</b>") + TRi(sw2, sw2 - 16, 14, "Esc", `color:#6B5846`) +
    T(16, 62, "Title", `color:#6B5846`) + B(16, 82, sw2 - 32, 30, `background:${L.cream};border:1px solid ${L.sand};border-radius:8px`, T(10, 7, "Dentist")) +
    T(16, 128, "When", `color:#6B5846`) + B(16, 148, 180, 28, `background:${L.cream};border:1px solid ${L.sand};border-radius:8px`, T(10, 6, "Wed 21 Oct")) + B(204, 148, 80, 28, `background:${L.cream};border:1px solid ${L.sand};border-radius:8px`, T(10, 6, "2:30 PM")) + T(292, 154, "to") + B(316, 148, 68, 28, `background:${L.cream};border:1px solid ${L.sand};border-radius:8px`, T(10, 6, "3:30")) +
    T(16, 192, "Repeat", `color:#6B5846`) + B(16, 212, 180, 28, `background:${L.cream};border:1px solid ${L.sand};border-radius:8px`, T(10, 6, "Never") + TRi(180, 170, 6, "&#9662;", `color:#6B5846`)) + T(204, 192, "Alert", `color:#6B5846`) + B(204, 212, 180, 28, `background:${L.cream};border:1px solid ${L.sand};border-radius:8px`, T(10, 6, "15 minutes before") + TRi(180, 170, 6, "&#9662;", `color:#6B5846`)) +
    T(16, 256, "Calendar", `color:#6B5846`) + circle(16, 282, 14, C.straw) + T(38, 280, "Personal") + T(204, 256, "Notes", `color:#6B5846`) + B(204, 278, 180, 54, `background:${L.cream};border:1px solid ${L.sand};border-radius:8px`, T(10, 6, "12 Elm St", `color:#6B5846`)) +
    B(0, sh - 52, sw2, 52, `border-top:1px solid ${L.sand}`) + btn(sw2 - 200, sh - 40, 88, "Cancel", "secondary") + btn(sw2 - 104, sh - 40, 88, "Save", "primary"));
  return h + appWin(x, y, w, hh, "Calendar", b + sheet);
}
function calAlert() {
  let h = base("Calendar") + dock([2, 4]) + dim(0.1);
  const t = TOK.toast, tx = 1024 - t.right - 300;
  h += glass(tx, t.top, 300, 92, TOK.radius.toast, icon(12, 14, 44, "cal", C.milk, C.esp, `border:1px solid ${C.sand}`) + T(66, 12, "<b>Dentist</b>") + TRi(300, 288, 12, "in 15 min", `color:#6B5846`) + T(66, 32, "2:30 PM, 12 Elm St", `color:#6B5846`) + pill(66, 56, 70, "Snooze", "secondary") + pill(144, 56, 78, "Open", "primary"));
  h += glass(48, 56, 300, 240, 20, T(16, 16, "NEXT UP", `color:#6B5846`) + T(16, 42, "<b>2:30 PM</b>", X2.replace("26.667px", "16px").replace("32px", "20px")) + T(16, 74, "Dentist") + T(16, 94, "12 Elm St", `color:#6B5846`) + B(16, 126, 268, 1, `background:${C.sand}`) + T(16, 138, "7:30 PM  Dinner") + T(16, 160, "Tomorrow  Study group", `color:#6B5846`) + T(16, 206, "From the Calendar", `color:#6B5846`));
  h += T(380, 90, "A reminder reaches you in three places:", `color:${C.esp};background:${C.milk}cc;border-radius:6px;padding:2px 6px`) + T(380, 114, "a toast, a card in the notification drawer, the Today widget", `color:${C.esp};background:${C.milk}cc;border-radius:6px;padding:2px 6px`) + T(380, 138, "and the soft two-note Calendar alert sound (Focus mutes it).", `color:${C.esp};background:${C.milk}cc;border-radius:6px;padding:2px 6px`);
  return h;
}

/* ================= CLOCK / TIMER ================= */
const clockTabs = (i) => B(0, 32, 600, 44, `background:${L.cream};border-bottom:1px solid ${L.sand}`) + seg(106, 40, ["Clock", "Alarms", "Stopwatch", "Timer"], i, 96);
function clkClock() {
  let h = base("Clock") + dock([5]);
  const x = 140, y = 70, w = 600, hh = 560;
  let b = clockTabs(0);
  b += circle(w / 2 - 110, 96, 220, L.cream, `border:3px solid ${L.espresso}`) + [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11].map((k) => { const a = k * 30 * Math.PI / 180, cx = w / 2 + 94 * Math.sin(a), cy = 206 - 94 * Math.cos(a); return circle(cx - 3, cy - 3, k % 3 === 0 ? 7 : 4, k % 3 === 0 ? L.espresso : L.latte); }).join("") +
    B(w / 2 - 2, 206 - 60, 4, 60, `background:${L.espresso};border-radius:2px`) + B(w / 2 - 2, 206 - 84, 3, 84, `background:${L.espresso};border-radius:2px;transform:rotate(246deg);transform-origin:50% 100%`) + B(w / 2, 206 - 92, 2, 92, `background:${C.straw};transform:rotate(42deg);transform-origin:50% 100%`) + circle(w / 2 - 7, 199, 14, C.straw, `border:3px solid ${L.milk}`);
  b += TC(0, 336, w, "10:41", "font-size:53.333px;line-height:64px;font-weight:700") + TC(0, 404, w, "Wednesday, 21 October", `color:#6B5846`);
  b += B(40, 440, w - 80, 1, `background:${L.sand}`) + T(40, 456, "WORLD CLOCKS (optional, fixed offsets)", `color:#6B5846`);
  [["Sydney", "+11 h", "21:41"], ["Dayton", "this machine", "10:41"], ["Reykjavik", "+4 h", "14:41"]].forEach((c, i) => { b += T(40 + i * 180, 482, `<b>${c[2]}</b>`) + T(40 + i * 180, 502, c[0], `color:#6B5846`) + T(40 + i * 180, 520, c[1], `color:#6B5846`); });
  return h + appWin(x, y, w, hh, "Clock", b);
}
function clkAlarms() {
  let h = base("Clock") + dock([5]);
  const x = 140, y = 70, w = 600, hh = 560;
  let b = clockTabs(1) + pill(w - 100, 44, 84, "New alarm") + "";
  [["7:00", "Weekdays", "Wake up", true], ["8:30", "Mon", "Bike repair shop", true], ["21:00", "Every day", "Wind down", false]].forEach((a, i) => {
    const ay = 92 + i * 84; b += B(16, ay, w - 32, 72, `background:${L.cream};border:1px solid ${L.sand};border-radius:14px;opacity:${a[3] ? 1 : 0.7}`, T(18, 10, `<b>${a[0]}</b>`, "font-size:40px;line-height:48px") + T(160, 14, a[2]) + T(160, 36, a[1], `color:#6B5846`) + B(w - 32 - 70, 24, 44, 24, `background:${a[3] ? C.straw : C.sand};border-radius:12px`, circle(a[3] ? 22 : 2, 2, 20, C.milk)));
  });
  b += T(16, 360, "Alarms keep ringing when the window is closed. A ringing alarm is the one sound Focus lets through.", `color:#6B5846`) + T(16, 380, "Next: 7:00 AM tomorrow, in 20 h 19 min.", `color:#6B5846`);
  return h + appWin(x, y, w, hh, "Clock", b);
}
function clkTimer() {
  let h = base("Clock") + dock([5]);
  const x = 140, y = 70, w = 600, hh = 560;
  let b = clockTabs(3);
  b += B(w / 2 - 130, 96, 260, 260, `background:conic-gradient(${C.straw} 0 62%, ${L.sand} 62% 100%);border-radius:50%`) + circle(w / 2 - 112, 114, 224, L.milk) + TC(0, 190, w, "04:32", "font-size:53.333px;line-height:64px;font-weight:700") + TC(0, 262, w, "Tea", `color:#6B5846`) + TC(0, 284, w, "ends at 10:46", `color:#6B5846`);
  b += btn(w / 2 - 190, 384, 120, "Reset", "secondary") + btn(w / 2 - 60, 384, 120, "Pause", "primary") + btn(w / 2 + 70, 384, 120, "+1 min", "secondary");
  b += T(16, 440, "PRESETS", `color:#6B5846`) + [["1 min", 0], ["5 min", 1], ["10 min", 2], ["25 min", 3], ["Custom", 4]].map((p, i) => chip(16 + i * 100, 462, 92, p[0], false)).join("");
  b += T(16, 510, "A running timer also shows in the menu bar so you can leave this window.", `color:#6B5846`);
  return h.replace(menubar("Clock"), menubar("Clock") + TC(430, 5, 160, "<b>&#9719; 04:32</b>", `background:${C.straw}55;border-radius:10px;color:${INK}`)) + appWin(x, y, w, hh, "Clock", b);
}
function clkStopwatch() {
  let h = base("Clock") + dock([5]);
  const x = 140, y = 70, w = 600, hh = 560;
  let b = clockTabs(2);
  b += TC(0, 110, w, "01:23.45", "font-size:53.333px;line-height:64px;font-weight:700") + btn(w / 2 - 190, 200, 120, "Lap", "secondary") + btn(w / 2 - 60, 200, 120, "Stop", "danger") + btn(w / 2 + 70, 200, 120, "Reset", "secondary");
  b += B(16, 260, w - 32, 28, `background:${L.oat};border-bottom:1px solid ${L.sand}`) + T(28, 266, "Lap", `color:#6B5846`) + T(220, 266, "Lap time", `color:#6B5846`) + T(420, 266, "Total", `color:#6B5846`);
  [["3", "00:21.10", "01:23.45"], ["2", "00:31.40", "01:02.35"], ["1", "00:30.95", "00:30.95"]].forEach((l, i) => { b += T(28, 298 + i * 30, l[0]) + T(220, 298 + i * 30, l[1], i === 0 ? `color:#4C7A55;${BD}` : "") + T(420, 298 + i * 30, l[2]); });
  b += T(16, 420, "Fastest lap in green, slowest in red. Uses the monotonic clock, so changing the time does not disturb it.", `color:#6B5846`);
  return h + appWin(x, y, w, hh, "Clock", b);
}

/* ================= PREVIEW ================= */
const photo = (x, y, w, hh) => B(x, y, w, hh, `background:linear-gradient(180deg,#F9D6B8,#F2A9A0 62%,#C9B3D9);overflow:hidden;border-radius:2px`, circle(w * 0.62, hh * 0.14, hh * 0.22, C.honey) + circle(-w * 0.2, hh * 0.58, w * 0.9, "#CFE0C4") + circle(w * 0.34, hh * 0.7, w * 0.9, "#B9D3B0") + B(w * 0.72, hh * 0.66, w * 0.14, hh * 0.07, "background:#fff;border-radius:4px"));
function prvImage() {
  let h = base("Preview") + dock([6]);
  const x = 80, y = 52, w = 860, hh = 660;
  let b = B(0, 32, w, 40, `background:${L.cream};border-bottom:1px solid ${L.sand}`) + pill(12, 40, 28, "&lt;", "secondary") + pill(44, 40, 28, "&gt;", "secondary") + T(82, 48, "bike.png   2 of 4", `color:#6B5846`) + seg(w / 2 - 190, 38, ["Fit", "25%", "50%", "100%", "200%"], 0, 62) + pill(w - 190, 42, 28, "&#8635;", "secondary") + pill(w - 156, 42, 28, "i", "secondary") + pill(w - 122, 42, 110, "Save as...", "secondary");
  b += B(0, 72, w, hh - 72 - 28, `background:${L.oat}`) + photo(130, 100, 600, 450);
  b += B(0, hh - 28, w, 28, `background:${L.cream};border-top:1px solid ${L.sand}`) + T(14, hh - 24, "800 x 600   1.0 MB   PNG   Zoom: Fit (75%)", `color:#6B5846`) + TRi(w, w - 14, hh - 24, "Drag to pan   Space: next   Cmd-0: fit", `color:#6B5846`);
  return h + appWin(x, y, w, hh, "bike.png", b);
}
function prvBrowse() {
  let h = base("Preview") + dock([6]);
  const x = 80, y = 52, w = 860, hh = 660, sw = 150;
  let b = B(0, 32, w, 40, `background:${L.cream};border-bottom:1px solid ${L.sand}`) + seg(w / 2 - 190, 38, ["Fit", "25%", "50%", "100%", "200%"], 4, 62) + T(14, 48, "trail.png", `color:#6B5846`) + pill(w - 122, 42, 110, "Save as...", "secondary");
  b += B(0, 72, sw, hh - 72 - 28, `background:${L.oat};border-right:1px solid ${L.sand}`);
  ["bike.png", "trail.png", "lake.png", "sun.png"].forEach((n, i) => { const ty = 84 + i * 108; b += (i === 1 ? B(8, ty - 4, sw - 16, 100, `background:${C.straw}33;border:2px solid ${C.straw};border-radius:10px`) : "") + photo(16, ty, sw - 32, 66) + TC(0, ty + 70, sw, n); });
  b += B(sw, 72, w - sw, hh - 72 - 28, `background:${L.oat}`) + B(sw, 72, w - sw, hh - 72 - 28, "overflow:hidden", photo(-60, -20, 1400, 1050));
  b += B(w - 150, hh - 28 - 100, 130, 90, `background:rgba(255,253,248,.92);border:1px solid ${L.sand};border-radius:8px`, photo(6, 6, 118, 78) + B(34, 24, 54, 36, `border:2px solid ${C.straw}`));
  b += B(0, hh - 28, w, 28, `background:${L.cream};border-top:1px solid ${L.sand}`) + T(14, hh - 24, "1600 x 1200   2.3 MB   PNG   Zoom: 200% (nearest-neighbour is fine here)", `color:#6B5846`);
  return h + appWin(x, y, w, hh, "trail.png", b);
}
function prvPdf() {
  let h = base("Preview") + dock([6]);
  const x = 140, y = 70, w = 700, hh = 520;
  let b = B(0, 32, w, 40, `background:${L.cream};border-bottom:1px solid ${L.sand}`) + T(14, 48, "invoice.pdf", `color:#6B5846`);
  b += B(0, 72, w, hh - 72, `background:${L.oat}`) + B(w / 2 - 180, 100, 360, 300, `background:${L.milk};border:1px solid ${L.sand};border-radius:14px`, icon(150, 24, 60, "doc", L.cream, L.esp, `border:1px solid ${L.sand}`) + TC(0, 106, 360, "<b>PDF preview is not available yet</b>") + TC(0, 128, 360, "3 pages, about 1,204 words", `color:#6B5846`) + TC(0, 150, 360, "Milk can read the text but not draw the pages.", `color:#6B5846`) + btn(60, 196, 110, "Show text", "primary") + btn(190, 196, 110, "Open in Editor", "secondary")) + T(w / 2 - 180, 420, "Images that can not be decoded say so the same way: \"Can not open this image: JPEG is not supported yet.\"", `color:#6B5846`).replace("white-space:pre", "white-space:normal;width:360px");
  return h + appWin(x, y, w, hh, "invoice.pdf", b);
}

/* ================= slides ================= */
const SLIDES = [
  { id: "am-procs", n: "1", name: "Activity Monitor: Processes", tag: "A table of what is running", draw: amProcesses, cost: "medium", fam: "A process table like Activity Monitor or top: name, PID, state, CPU %, memory, threads, with a search box and a Quit button.", nw: "A Sandbox column shows which apps are confined, and the detail panel says in words what the selected app can read, whether it has network, and what it was refused today.", helps: "Anyone asking what is using the computer, and whether an app is behaving.", why: "Reads /proc once a second. See SPEC-APPS.md.", els: ["Tabs", "Process table", "Sandbox chip", "Detail panel", "Summary bar"], font: "Right-align numbers so the digits line up." },
  { id: "am-cpu", n: "2", name: "Activity Monitor: CPU", tag: "Per-core graphs", draw: amCpu, cost: "easy", fam: "Four live graphs, one per core, plus load.", nw: "Sixty seconds of history per core, load averages, the busiest processes, and interrupt rates.", helps: "Spotting a runaway process or a busy core.", why: "From /proc/cpuload, /proc/loadavg and /proc/interrupts.", els: ["Per-core charts", "Load line", "Busiest list", "Interrupt rates"], font: "Charts are rectangles, one column per second." },
  { id: "am-mem", n: "3", name: "Activity Monitor: Memory", tag: "Where the memory went", draw: amMemory, cost: "easy", fam: "A stacked bar of used and free memory, pressure, and the biggest users.", nw: "Page cache is shown separately (memory the system can give back) so a full-looking bar does not worry anyone.", helps: "Understanding memory without a lecture.", why: "From /proc/meminfo and each process's VmRSS.", els: ["Stacked bar", "Pressure gauge", "Biggest users"], font: "One headline sentence in 2x text." },
  { id: "am-sec", n: "4", name: "Activity Monitor: Security", tag: "Sandbox denials and audit events as sentences", draw: amSecurity, cost: "medium", fam: "A timeline with filter chips, like an activity feed.", nw: "The audit log read as sentences (peek was refused /etc/passwd, You gave peek a file, moo2048 started sandboxed). A green line says the log is intact, from the tamper-evident chain.", helps: "Seeing what apps tried to do, and trusting the record.", why: "Reads /proc/audit (permission being added) and runs the audit verify check. Read only.", els: ["Intact banner", "Filter chips", "Event timeline", "Raw record on click"], font: "Dots and the line carry the look." },
  { id: "te-edit", n: "5", name: "Text Editor", tag: "Plain text with line numbers and light colour", draw: () => teEdit(false, false), cost: "medium", fam: "A plain editor: gutter, current-line highlight, status bar.", nw: "Light syntax colours that all pass 4.5:1 in both themes, soft wrap, undo that groups typing, and Versions from the filesystem for free.", helps: "Writing code and notes without an IDE.", why: "Starts from scratch: line layout, wrapping and undo are new work for the app engineer.", els: ["Gutter", "Current line", "Syntax colours", "Status bar"], font: "Unifont at 8x16 with 18 px line height." },
  { id: "te-find", n: "6", name: "Text Editor: Find and Replace", tag: "A bar under the title", draw: () => teEdit(false, true), cost: "easy", fam: "Cmd-F opens a find bar with next and previous and a match count.", nw: "All matches are tinted honey and the current one straw. Replace and Replace All sit in the same bar, no dialog.", helps: "Fast edits.", why: "A search over the buffer and a highlight layer.", els: ["Find field", "Count", "Prev/next", "Case toggle", "Replace"], font: "Short labels only." },
  { id: "te-tabs", n: "7", name: "Text Editor: tabs and Markdown", tag: "Many files, one window", draw: teTabs, cost: "medium", fam: "Tabs as in the Tabs spec; the dot on a tab means unsaved changes.", nw: "Markdown gets light styling (bold headings, a coloured bullet, a tinted code span) without rendering it. Autosave is off by default for files; Notes keeps its autosave.", helps: "Working on several files.", why: "Uses the shared tab strip; Markdown styling is a line-based pass.", els: ["Tab strip", "Unsaved dot", "Markdown styling", "Soft wrap"], font: "Bold is the faux bold." },
  { id: "cal-month", n: "8", name: "Calendar: Month", tag: "The familiar month grid", draw: calMonth, cost: "medium", fam: "A month grid with coloured events, a mini calendar and a list of calendars on the left.", nw: "Event chips use the calendar's colour as a tint with dark text, so they stay readable in both themes. Today is a straw circle with a dark numeral.", helps: "Seeing the month at a glance.", why: "Simple data file plus a grid renderer.", els: ["Sidebar", "Month grid", "Event chips", "+N more", "View switch"], font: "Event text at the 12 px size is cut with a tilde." },
  { id: "cal-week", n: "9", name: "Calendar: Week and new event", tag: "Hours down the side, a sheet to add events", draw: calWeek, cost: "medium", fam: "A week with an hour grid and a now line; a sheet for new events.", nw: "The sheet has title, when, repeat, alert, calendar and notes, nothing more. The alert you choose becomes a notification, a Today card and a soft sound.", helps: "Planning a week.", why: "Event fields and a repeat expander; alerts go through the notification drawer.", els: ["Hour grid", "Now line", "Event sheet", "Alert picker"], font: "Fields are 28 px high." },
  { id: "cal-alert", n: "10", name: "Calendar: Alert", tag: "Toast, drawer card, Today card", draw: calAlert, cost: "easy", fam: "A reminder pops up.", nw: "Snooze and Open on the toast. The next event appears on the Today card, which replaces the empty \"No calendar events here yet\" line.", helps: "Not missing the dentist.", why: "Uses the toast and drawer already specified.", els: ["Toast with Snooze/Open", "Drawer card", "Today card", "Alert sound"], font: "One line of text per row." },
  { id: "clk-clock", n: "11", name: "Clock", tag: "Local time, with optional world clocks", draw: clkClock, cost: "easy", fam: "An analog and a digital clock.", nw: "Local time only to start (the machine has a real-time clock but no time zone data). World clocks are optional and use a small table of fixed offsets, no daylight saving, and say so.", helps: "A clock you can read at a glance.", why: "Wall clock via the RTC.", els: ["Analog face", "Digital time", "World clocks (optional)"], font: "The time at 4x." },
  { id: "clk-alarms", n: "12", name: "Clock: Alarms", tag: "A short list with switches", draw: clkAlarms, cost: "medium", fam: "A list of alarms with on/off switches.", nw: "Alarms keep ringing when the window is closed, and a ringing alarm is the one thing that gets through Focus.", helps: "Waking up.", why: "Needs the app to stay alive without a window, like Music.", els: ["Alarm rows", "Switches", "Next-alarm line"], font: "Time at 40 px." },
  { id: "clk-timer", n: "13", name: "Clock: Timer", tag: "A ring that empties", draw: clkTimer, cost: "easy", fam: "A countdown with a progress ring and presets.", nw: "A running timer also shows in the menu bar, so you can leave the window. When it ends: a toast and a three-note sound.", helps: "Tea, cooking, focus sessions.", why: "Monotonic clock; the menu bar item is a small status area addition.", els: ["Ring", "Start/pause/reset", "Presets", "Menu bar timer"], font: "Remaining time at 4x." },
  { id: "clk-stop", n: "14", name: "Clock: Stopwatch", tag: "Laps", draw: clkStopwatch, cost: "easy", fam: "A stopwatch with laps.", nw: "Fastest lap in green and slowest in red (also shown with an arrow, not colour alone).", helps: "Timing things.", why: "Monotonic clock.", els: ["Time", "Lap/Stop/Reset", "Lap table"], font: "Tabular digits." },
  { id: "prv-img", n: "15", name: "Preview: image", tag: "Open, zoom, pan", draw: prvImage, cost: "medium", fam: "A viewer with a toolbar, zoom steps and an info line.", nw: "Zoom is stepped (Fit, 25, 50, 100, 200%) so scaling stays simple. It opens only what you choose through the Open panel or drag, which makes it a good example of a sandboxed app.", helps: "Looking at pictures.", why: "Needs the PNG/PPM/BMP decoders; scaling is done by the app (box or nearest).", els: ["Toolbar", "Zoom steps", "Rotate", "Info", "Save as"], font: "Status line in secondary text." },
  { id: "prv-browse", n: "16", name: "Preview: several images", tag: "A thumbnail strip", draw: prvBrowse, cost: "medium", fam: "Thumbnails on the left, a large image, a mini map when zoomed.", nw: "Arrow keys move through the images in the folder. At 200% a small map shows where you are.", helps: "Flicking through photos.", why: "Thumbnails are decoded and scaled once and cached.", els: ["Thumbnails", "Selected outline", "Mini map"], font: "Names under thumbnails." },
  { id: "prv-pdf", n: "17", name: "Preview: unsupported files", tag: "Honest empty states", draw: prvPdf, cost: "easy", fam: "A page that says it can not open the file, and why.", nw: "PDF: the page count and a text extract, but no page rendering yet. A JPEG before the decoder exists says so in plain words.", helps: "Never a blank window.", why: "Text extract only; no PDF renderer.", els: ["Message", "Show text", "Open in Editor"], font: "Two lines." },
];

/* ================= SPEC-APPS.md ================= */
const SECOND = (a, b) => ratio(a, b).toFixed(2);
const synRows = (S, theme) => ["keyword", "string", "number", "comment", "error", "text"].map((k) => `| ${k} | ${S[k]} | ${SECOND(S[k], S.bg)}:1 | ${SECOND(S[k], theme === "dark" ? "#382D27" : "#F6EFE0")}:1 |`).join("\n");
const chipRows = [["straw", L.straw], ["matcha", L.matcha], ["blue", L.blue], ["honey", L.honey], ["lavender", L.lav]].map(([n, c]) => `| ${n} | ${SECOND(L.espresso, mixHex(L.milk, c, 0.27))}:1 | ${SECOND(DARK.espresso, mixHex(DARK.milk, c, 0.27))}:1 |`).join("\n");
const chartInk = { blue: "#4B63A8", honey: "#A87812", matcha: "#5B8A5E", straw: "#C9566B" };
const chartRows = Object.entries(chartInk).map(([n, c]) => `| ${n} | ${c} | ${SECOND(c, L.cream)}:1 | ${SECOND(TOK.color[n]?.[0] ?? L[n], DARK.cream)}:1 (the plain accent ${L[n]}) |`).join("\n");

const spec = `# milkOS app specs: Activity Monitor, Text Editor, Calendar, Clock/Timer, Preview

Generated by \`apps.mjs\` (the contrast numbers are computed from the tokens in \`SPEC.md\`). Mockups: \`apps.html\`. Read \`SPEC.md\` first (tokens, dark theme, Buttons, tabs, the notification drawer) and \`ART.md\` (icons). Written for **the app engineer** (the apps) and **the window-system engineer** (anything that touches the window API or the menu bar). Only what exists today is assumed: where something is missing, it is listed under "Needs" so it can be scheduled.

## What the system can and can not do today (facts the specs are built on)
- **Images:** there is a PNG *encoder* (for screenshots) and WAV decoding, but **no image decoder**. PNG and PPM/BMP decoders are being written; JPEG later or never. No PDF rendering at all.
- **Scaling:** the compositor has a scaled blit, but it is in the kernel, not in the user GUI library. Userland apps do their own nearest or box scaling, so zoom is stepped, never continuous.
- **Fonts:** bitmap faces only (Unifont 8x16, smooth 2x/4x/8x scaling, and a proportional UI variant in the kernel window code). The user GUI library has plain Unifont 8x16 (scale with the text-at-scale call). No vector fonts, no font loader. A new font needs the project owner's yes.
- **Time:** a wall clock from the real-time clock (local time) and a monotonic clock. **No time zone data, no internet time.**
- **User data:** \`~/.config/milk/\` for settings and \`~/Library/<app>/\` for app data (sandboxed packaged apps get exactly that folder, read-write). It lives on milkfs, so it persists and gets Time Machine and Versions for free. No calendar file format exists yet.
- **Processes:** \`/proc/<pid>/status\` (name, state, pid, parent, thread count, resident memory, CPU time, start time), \`/proc/<pid>/{stat,cmdline,maps,fd,cwd,exe}\`, \`/proc/threads\`, \`/proc/stat\` and \`/proc/cpuload\` (per CPU), \`/proc/loadavg\`, \`/proc/meminfo\` (total, free, kernel heap, page cache), \`/proc/diskstats\`, \`/proc/interrupts\`, \`/proc/audit\`. **Capabilities are not in /proc yet** (a Caps line is requested).
- **Text layout:** the GUI library has no line-wrapping helper; the Editor's app engineer builds one.

## Conventions for every app below
- **Window:** a standard window (SPEC "Window chrome"). Default size and minimum size are given per app. Position cascades 24px from the last window.
- **Toolbar:** 40 high under the title bar, cream, 1px sand line below. Segmented control: height 28, radius 14, oat fill, the selected segment milk with a sand border. Search field: 24 high, radius 12.
- **Status bar:** 24 high, cream, 1px sand line above, secondary text.
- **Buttons:** only the kinds in SPEC "Buttons". Destructive actions use the Danger kind and ask first.
- **Empty states:** one calm sentence in secondary text and, if there is an obvious next step, one primary button. Never a blank window.
- **Text:** primary and secondary text colours only on their surfaces; latte is for disabled text. Numbers right-aligned with fixed-width digits (the bitmap font already has them). Truncate with "~" (a real ellipsis if the font draws it).
- **Keys:** Cmd-W closes, Cmd-Q quits, Cmd-F finds, Cmd-1..9 switch tabs or segments, arrow keys move selection, Return opens, Esc closes sheets and clears search. Every action is reachable from the keyboard.
- **Sandbox:** packaged apps run sandboxed. They see only their own folder plus files given through the Open panel or drag and drop. Specs say which extra access each app needs.
- **Themes:** all colours come from tokens, so the dark theme is automatic; where a spec adds a colour it gives both themes and the contrast.

---

## 1. Activity Monitor
**Purpose:** show what is running and how much it uses, and let the user quit something that is stuck, plus a readable view of sandbox denials and audit events.
**Window:** 800 x 640 default, 560 x 360 minimum. Toolbar with a segmented control: **Processes, CPU, Memory, Disk, Security**. Cmd-1 to Cmd-5 switch. Refresh every 1 second (Settings: 0.5, 1, 2, 5, or paused).

### Processes
- **Table** (header 28, rows 28, zebra at 35% oat, selected row straw at 20% with primary text only). Columns: Name (20px app icon + name), PID, State, CPU %, Memory, Threads, Sandbox. Click a header to sort (arrow in the header; default CPU % descending). Search filters by name.
- **Data:** Name and State from /proc/<pid>/status; PID; CPU % from the change in CpuNs between refreshes divided by wall time and the number of CPUs (show per-core percent, 100% = one core); Memory from VmRSS in MB with one decimal; Threads; Sandbox from the process's capability set: "Sandboxed" (matcha at 33%, dark text) when its file access is restricted, "System" (oat) for system processes, empty otherwise. Until the Caps line exists, derive nothing and leave the column hidden.
- **Detail panel** (appears under the table when a row is selected, 150 high, cream, radius 12): name, pid, start time, parent; executable path; plain-English **Can read** (its own folder, N files you chose, everything), **Network** (none, local only, yes), **Refused** (count today and the last refusal, in Danger text, from the audit log); open files count and threads; buttons **Show Files** (open its working folder in Files) and **Security** (jump to the Security tab filtered to this process).
- **Quit:** the Quit button (Danger pill, top right) asks to end the selected process: "Quit moo2048?" with Quit (sends the polite signal) and, if it does not exit in 5 seconds, a **Force Quit** button (kill). System processes (the window server, init) show the button disabled with the reason "System processes can not be quit". Keyboard: Delete asks the same question.
- **Summary bar:** "10 processes   14 threads   Memory 124 MB of 512 MB   CPU 11%" and "updates every 1 s".
- **Needs:** read access to every process's /proc entry (works today because everything runs as root; the real accounts work must keep this for the milk user), the Caps line, and a signal call.

### CPU
Four cards (one per core) in a 2 x 2 grid, each a 60-second chart (one column per second, 2 px wide) with the current percent at the top right. Beneath: load averages (1, 5 and 15 minutes), the three busiest processes now, and interrupt rates from /proc/interrupts (timer, network, input, sound per second). Data: /proc/cpuload, /proc/loadavg, /proc/interrupts.

### Memory
A headline sentence ("124 MB used of 512 MB") in 2x text, then a stacked bar (28 high, radius 14): **Apps** (honey), **Kernel heap** (blue), **Page cache** (lavender), **Free** (oat). The legend gives each in MB. **Memory pressure** is a gauge with Low (matcha) under 60% used, Medium (honey) 60 to 85%, High (danger) above 85%, where used = total - free - page cache. Below, the six biggest processes by resident memory with bars. Data: /proc/meminfo and VmRSS. A footnote explains that the page cache is memory the system can give back.

### Disk
Reads and writes per second as two charts (blue and straw), free space per mounted volume as gauges, from /proc/diskstats and the filesystem free counts.

### Security
The audit log as a timeline of plain-English events, read only. At the top a banner: **"Log is intact"** (matcha at 13%, matcha border) with the record count and the time of the last verification, and a Verify again button (secondary) that runs the same check as \`audit verify\`; if the chain is broken the banner is Danger: "The log has been changed", with the first bad record. Filter chips: All, Refused, Files, Programs, Installs, Milk Drop. Search. Each event: a coloured dot (Danger for refusals, matcha for grants and starts, blue for installs, honey for trust decisions, latte for ordinary programs), a bold sentence, a secondary line with the detail, the time on the right and the raw event name (e.g. sandbox.deny) under it. Clicking an event shows the raw record.
| Raw event | Sentence |
|---|---|
| sandbox.deny | "peek was refused /etc/passwd" |
| powerbox.grant | "You gave peek a file" (path in the detail) |
| sandbox.enter | "moo2048 started, sandboxed" |
| exec | "Terminal started a program" |
| user.pkg.install | "fortune was installed" |
| user.drop.recv / send | "Milk Drop received bike.png from alice" |
| user.drop.trust | "bob's key was trusted" |
| user.drop.key_changed | "bob's key changed" (Danger) |
| audit.tamper | "The log has been changed" (Danger) |
Unknown event names show as the raw name so nothing is hidden. **Needs:** /proc/audit readable by the app (permissions are being added), and the verify check callable from the app.

### Colours for charts (non-text, 3:1 against the background)
The plain accents are too pale to draw thin lines on cream, so charts use darker "ink" versions in the light theme and the plain accents in dark.
| Series | Light theme colour | Contrast on cream | Dark theme |
|---|---|---|---|
${chartRows}
Fills under lines use the plain accent at 30% so the line stays the carrier of meaning. Never use colour alone: every series is labelled.

---

## 2. Text Editor
**Purpose:** edit plain text and code. Not a word processor.
**Window:** 700 x 540 default, 360 x 240 minimum. No toolbar by default (the file name is in the title bar; a dot after the name, or on its tab, means unsaved changes). Several files open as tabs (SPEC "Tabs"; 38px strip when two or more).
**Layout (top to bottom):** title or tab strip, optional **find bar** (40 high), the text area, the status bar (24 high).
- **Gutter:** 48 wide, cream, 1px sand line on its right, line numbers right-aligned in secondary text 10px from the edge. Toggle in View (default on).
- **Text area:** milk, 12px padding left of the text, text at 8x16 with an **18px line height**. Current line highlighted with a 4% darker fill (#F6EFE0 light, #382D27 dark). The caret is a 2px primary-text bar; it blinks at 1 Hz and stays solid while typing. Selection is straw at 33% with primary text.
- **Soft wrap:** off for code files, on for text and Markdown (View > Wrap). Wrapped continuation lines have no number. Tab key inserts spaces (setting: 2 or 4, default 4).
- **Status bar:** "Ln 17, Col 5    UTF-8    LF    Spaces: 4    Rust" on the left, "Saved" or "Edited" on the right, secondary text. Clicking a field opens its menu (encoding is UTF-8 only for now, so that field is informational).
- **Files:** Cmd-O opens the Open panel (the Powerbox look when sandboxed), Cmd-S saves, Cmd-Shift-S Save As. **Autosave is off for files** (explicit save, with the dot); Notes keeps autosaving. Versions and Time Machine work on saved files because they are on milkfs. Closing with unsaved changes asks: Save, Don't Save, Cancel.
- **Undo and redo:** Cmd-Z, Cmd-Shift-Z, unlimited within the session. Typing groups into one step until a pause of 1 second, a newline, or a cursor jump.
- **Find and replace** (Cmd-F; Cmd-Option-F adds Replace): a bar under the title. Find field (150 wide), match count "2 of 3", previous and next, a case toggle "Aa", a Replace field (150 wide), Replace and All. All matches are tinted honey at 40%, the current match straw at 40% with a 1px straw border. Return = next, Shift-Return = previous, Esc closes and keeps the selection on the match. Go to line: Cmd-L.
- **Editing basics:** copy, cut, paste through the system clipboard; select all; double-click selects a word, triple-click a line; Home and End; Option-arrows by word; auto-indent on Return; matching brackets are drawn with a 1px honey box when the caret is next to one.
- **Syntax colours** (optional, by file extension: Rust, C, shell, Markdown, plain). Keep it to five roles so any theme works. All text colours pass 4.5:1 on the editor surface and on the current-line fill.
| Role (light) | Colour | On editor surface | On current line |
|---|---|---|---|
${synRows(SYN_L, "light")}
| Role (dark) | Colour | On editor surface | On current line |
|---|---|---|---|
${synRows(SYN_D, "dark")}
- **Markdown** (files ending in .md or .markdown): lines starting with # are bold (no size change), list markers are coloured straw-dark (#B5465B light, #E58C97 dark), inline code gets an oat tint, bold text between ** is bold. No rendering.
- **Sandbox:** its own folder plus files given through the Open panel or drag and drop. Needs the Open and Save panels.
- **Needs (app engineer):** a text buffer with undo, a line-wrapping layout, the syntax pass (a small tokenizer per language), find and replace. **Window-system engineer:** none beyond the tab strip.

---

## 3. Calendar
**Purpose:** see and plan days; remind. Local time only (no time zones, no sync).
**Window:** 940 x 676 default, 640 x 420 minimum. Sidebar 172 wide (oat): the current month as a mini calendar, the list of calendars (colour dot, name, click to show or hide), and a New event button. Toolbar 40: segmented **Day, Week, Month, Year**, the title ("October 2026" or "19 to 25 October") in 2x, Today, previous and next, search.
- **Month view:** 7 columns, 5 or 6 rows, each cell the full width and 112 high, 1px sand lines, days outside the month in latte with the cell at 45% oat. The day number is at the top left. **Today** is a 24px straw circle behind the number with **ink** numeral (ink on straw is 5.33:1; cream on straw is only 2.42:1). Events are chips 17 high with a 3px left bar in the calendar colour, fill the calendar colour at 27%, text 12px primary. Up to four per cell, then "+N more" (click to open that day).
- **Week view:** hour rows 48 high from 08:00 to 18:00 (scrolls), the hour label right-aligned in secondary text at the left, a **now line** (2px straw with a 10px dot), events as blocks with the same left bar and tint, the selected one with a 2px border in its colour. Day columns have a header "Wed 21" (bold for today, with a 3px straw underline).
- **Day view:** the same as a single wide column. **Year view:** twelve mini months; click a day to open it.
- **New event sheet** (400 x 400, SPEC sheet look, opens centred over the window): **Title**, **When** (date, start, end, "All day" switch), **Repeat** (Never, Daily, Weekly, Monthly, Yearly, with an "until" option), **Alert** (None, At time, 5 minutes before, 15, 1 hour, 1 day), **Calendar** (picker), **Notes**. Buttons Cancel (secondary) and Save (primary) on the right. Return saves, Esc cancels. Double-click a day or drag across hours to start one with times filled in.
- **Calendars:** Personal (straw), Home (matcha), Work (blue); more can be added with any token accent. Contrast of primary text on a calendar tint (27% over the surface):
| Calendar colour | Light theme | Dark theme |
|---|---|---|
${chipRows}
- **Alerts:** at the chosen time the Calendar posts a notification (the toast, with **Snooze** and **Open**), a card in the notification drawer, the **Calendar alert** sound (two soft notes), and the event appears on the **Today** card (which replaces "No calendar events here yet"). Focus mutes the sound and the toast but keeps the card.
- **Data:** one file per calendar in \`~/Library/Calendar/\`, plain text, one block per event (id, title, start, end or all-day, repeat rule, alert offset, notes), human-readable so Versions and Time Machine diff nicely. The app engineer defines the exact format; \`.ics\` import is a nice-to-have, export likewise. Dates are local; a repeat expands on the fly (no stored copies).
- **Needs:** the file format and parser (app engineer), a timer/alert agent that runs without the window (see Clock), the notification API (window-system engineer) and the Today card hook.

---

## 4. Clock and Timer
**Purpose:** a clock, alarms, a stopwatch and a timer.
**Window:** 600 x 560 default (also fine at 420 x 420 minimum). Toolbar 40 with the segmented control **Clock, Alarms, Stopwatch, Timer**.
- **Clock:** an analog face (220 px, 3px espresso ring, 12 ticks, the major ones solid, hour and minute hands espresso 4 and 3 px, a straw second hand with a straw hub ringed in milk), the time below at **4x** ("10:41"), the date in secondary text. Beneath, **World clocks** (optional in the first version): up to 3 cities as time, name and offset. **Local time only to start**: the machine has no time zone data, so world clocks use a small fixed table of offsets with no daylight saving, and say "fixed offsets" in the label.
- **Alarms:** a list of rows (72 high, cream, radius 14): the time at 40px, the label, the repeat ("Weekdays"), and an on/off switch (44x24, straw when on; off rows at 70% opacity). New alarm button top right. A line under the list shows "Next: 7:00 AM tomorrow, in 20 h 19 min". Alarms **keep working when the window is closed** (the app stays alive without a window, like Music). A ringing alarm shows a Danger-free, friendly panel: time, label, **Snooze 9 min** and **Stop**, and plays the alarm sound on a 2 second loop. It is the one sound Focus lets through.
- **Stopwatch:** 01:23.45 at 4x, Lap, Stop (Danger while running), Reset. A lap table: lap number, lap time, total. Fastest lap in green (#42704B on milk) and slowest in red (#B5473B, 5.26:1), **and** marked with an up or down arrow so colour is never the only cue.
- **Timer:** a ring 260 px (straw for the remaining time on a sand track), the remaining time at 4x in the middle with an optional label ("Tea") and "ends at 10:46" beneath, buttons Reset (secondary), Pause (primary) and +1 min (secondary), and presets as chips (1, 5, 10, 25 minutes, Custom). A running timer **also shows in the menu bar status area** as "04:32" with a small straw tint, so the window can be closed. When it ends: a toast, a card in the drawer, and the **Timer done** sound (three notes, twice).
- **Timing:** stopwatch and timer use the **monotonic clock**, so changing the wall clock does not disturb them. Alarms use the wall clock.
- **Data:** \`~/Library/Clock/\` (alarms, last timer presets). Plain text.
- **Needs:** a background agent (alarms and timers must survive a closed window), notification and sound hooks, the menu bar timer item (window-system engineer).

---

## 5. Preview
**Purpose:** look at images (and say honestly when it can not).
**Window:** 860 x 660 default, 420 x 320 minimum. Toolbar 40: previous and next, the file name and "2 of 4" when a folder's images are being browsed, a segmented **zoom** control **Fit, 25%, 50%, 100%, 200%**, a rotate button, an info button and **Save as...**. Status bar: "800 x 600   1.0 MB   PNG   Zoom: Fit (75%)" and key hints.
- **Canvas:** oat background behind the image, the image centred with no border. Drag to pan when larger than the window; Space or the arrows move between files in the same folder; Cmd-0 fits; Cmd-+ and Cmd-- step through the zoom list.
- **Zoom:** only the five steps above. **Fit** scales down to the window and never above 100%. 25 and 50% use a box average, 100% is exact, 200% uses nearest neighbour (crisp pixels are the right answer for a pixel-art OS). The scaling is done by the app (the compositor's scaled blit is kernel only).
- **Several images:** a thumbnail strip, 150 wide, in oat on the left; thumbnails 118 x 66, selected one with a straw 2px outline and 10px radius, name below. Thumbnails are decoded and scaled once and cached in memory. When zoomed beyond the window, a mini map (130 x 90, bottom right) shows the visible area.
- **Info:** the i button toggles a small panel with size in pixels, file size, format, and modification time.
- **Save as:** saves a PNG (the screenshot encoder exists), also useful for converting a BMP or PPM.
- **Formats:** PNG, PPM, BMP first. JPEG later or not at all. **PDF:** a card that says "PDF preview is not available yet", shows the page count and the amount of text, and offers **Show text** (the extracted text in a read-only view) and **Open in Editor**. An undecodable image says "Can not open this image: JPEG is not supported yet." in the same card style. Never a blank window.
- **Sandbox:** it can read only the files you give it (Open panel, drag and drop, or "Open with" from Files), which makes it the model sandboxed app: it needs no extra grants. Save as uses the Save panel.
- **Needs (app engineer):** the PNG, PPM and BMP decoders, nearest and box scaling, thumbnail caching. **Window-system engineer:** none.
`;
const here = dirname(fileURLToPath(import.meta.url));
writeFileSync(join(here, "SPEC-APPS.md"), spec);

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



writeFileSync(join(here, "apps.html"), buildPage(SLIDES, {
  title: "milkOS: built-in apps", h1: "milkOS: five built-in apps (mockups)",
  sub: "Activity Monitor, Text Editor, Calendar, Clock and Timer, and Preview, designed to what the system can do today. The spec is SPEC-APPS.md.",
  planTitle: "Notes for the engineers",
  plan: "<p>SPEC-APPS.md lists, per app, what to build and what each needs (the app engineer and the window-system engineer). Everything is read from existing /proc files, the real-time clock and the monotonic clock. Not assumed: image decoders beyond the ones being written, time zone data, PDF rendering, vector fonts.</p>",
  file: "apps.html" }));
console.log("wrote apps.html and SPEC-APPS.md");
