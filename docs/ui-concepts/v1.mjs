// milkOS 1.0: the merged concept (01 Whole Milk shell + 03 Skim launcher + 06 Today home + 07 Spaces + 10 Glass look).
// Every number the mockup uses comes from TOK, and SPEC.md is generated from the same TOK, so they cannot drift.
// Run: node docs/ui-concepts/v1.mjs   (writes milkos-1.0.html and SPEC.md)
import { B, T, TC, TR, TRi, icon, drop, X2, BD } from "./scenes.mjs";
import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

export const TOK = {
  screen: { w: 1024, h: 768, grid: 4 },
  color: {
    milk: ["#FFFDF8", "window body, cards, launcher"],
    cream: ["#FBF6EC", "title bars, page background"],
    oat: ["#EFE6D4", "sidebars, inactive fills"],
    sand: ["#E2D5BD", "hairlines, borders, tracks"],
    latte: ["#C4AE8F", "disabled text, inactive lights"],
    cocoa: ["#6B5846", "secondary text (was #7A6652: 4.40:1 on the sidebar, now 5.45:1)"],
    espresso: ["#3A2E28", "primary text, terminal background"],
    straw: ["#E58C97", "accent (selection, close light, progress)"],
    honey: ["#EBB650", "focus ring, minimise light, warnings"],
    matcha: ["#9CBF9B", "success, zoom light, good meters"],
    blue: ["#7F93C9", "info, links, disk meter"],
    lav: ["#B9A9D6", "wallpaper only"],
  },
  text: {
    ui: "Unifont 8x16, 1x, espresso. Secondary text cocoa. Disabled latte.",
    bold: "Same glyphs drawn twice, second copy at +1px x (faux bold). Nicer font later: real bold.",
    heading: "Unifont at 2x (16x32), bold.",
    display: "Unifont at 4x (32x64), bold. Clock only.",
    note: "No text smaller than 16px tall. Small caps labels are 8x16 uppercase in cocoa, not a smaller size.",
  },
  radius: { window: 12, dock: 20, launcher: 18, card: 20, toast: 16, button: 8, row: 12, chip: 8, tile: "24% of icon size", light: "circle d=12" },
  menubar: { h: 28, padX: 12, itemGap: 24, textY: 6, fillAlpha: 0.55, borderBottom: "1px sand" },
  dock: { h: 64, pad: 8, icon: 48, gap: 12, bottom: 12, dot: 4, dotGap: 4, fillAlpha: 0.55 },
  window: { titleH: 32, lightD: 12, lightGap: 8, lightX: 14, lightY: 10, border: "1px sand", min: [240, 160], bodyPad: 16, sidebarW: 160, statusH: 24 },
  glass: { blurRadius: 24, blurPasses: 3, fillAlpha: 0.55, borderAlpha: 0.75, border: "1px white", rule: "Only panels directly over the wallpaper are glass (menu bar, dock, launcher, toasts, control centre, home cards). Windows are opaque." },
  shadow: {
    color: "espresso (58,46,40)",
    window: [{ grow: 10, dy: 12, a: 0.05 }, { grow: 5, dy: 8, a: 0.07 }, { grow: 0, dy: 4, a: 0.12 }],
    float: [{ grow: 6, dy: 6, a: 0.05 }, { grow: 3, dy: 4, a: 0.07 }],
    how: "Stack the listed rounded rects (grown by `grow` px on every side, moved down `dy`, alpha `a`). No blur needed.",
  },
  launcher: { w: 640, y: 140, inputH: 56, rowH: 48, gap: 12, maxRows: 6, dim: 0.25, iconIn: 40, iconRow: 36 },
  toast: { w: 264, h: 64, right: 12, top: 40 },
  tabs: { stripH: 38, tabH: 32, tabY: 6, firstX: 84, minW: 96, maxW: 160, gap: 6, padL: 12, padR: 8, radiusTop: 10, closeHit: 20, plusW: 28, plusH: 24, plusR: 8, dropBorder: "2px dashed straw", dropFill: "straw at 20%" },
  button: { pillH: 24, pillR: 12, dialogH: 28, dialogR: 8, padX: 14, minW: 88, ink: "#3A2E27", hover: "#EA9AA4", pressed: "#DE7D89", danger: "#B5473B" },
  files: { toolbarH: 40, navD: 24, pathH: 24, searchW: 150, sidebarW: 168, statusH: 24 },
  menuStatus: { glyph: 16, gap: 12, hitH: 28 },
  drawer: { w: 316, right: 12, top: 36, bottom: 12, pad: 12, cardH: 64, cardCompactH: 56, cardR: 14, iconIn: 36, headerH: 78, chipH: 26, dndH: 44, alpha: 0.78, toastSecs: 5 },
  control: { w: 288, h: 292, right: 12, top: 36, tileH: 56, sliderH: 8 },
  cards: { pad: 16, gap: 16, r: 20, cols: 3 },
  spaces: { stripY: 40, thumbW: 128, thumbH: 88, thumbGap: 12, thumbR: 10, dim: 0.35, selBorder: 4, windowScale: "≈0.29 (scaled blit)" },
  wallpaper: "Dawn pasture: vertical gradient #FBE3C8 → #F3C3B8 → #C9B3D9, honey sun disc with halo, three overlapping matcha hill circles, tiny cow. Stored sharp (W) and pre-blurred (Wb = box blur r24 x3 passes, recomputed on change).",
};
const K = TOK.color;
const col = (n) => K[n][0];

/* ------------------------------ drawing kit ------------------------------ */
const hex2rgb = (h) => [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16));
const rgba = (h, a) => `rgba(${hex2rgb(h).join(",")},${a})`;
const ESP = "rgba(58,46,40,";

function wallpaper() {
  let h = B(0, 0, 1024, 768, `background:linear-gradient(180deg,#FBE3C8 0%,#F3C3B8 52%,#C9B3D9 100%)`);
  h += B(700, 40, 240, 240, `border-radius:50%;background:rgba(255,253,248,.28)`);
  h += B(740, 80, 160, 160, `border-radius:50%;background:${col("honey")}`);
  h += B(-120, 500, 1040, 1040, `border-radius:50%;background:#CFE0C4`);
  h += B(380, 590, 1100, 1100, `border-radius:50%;background:#B9D3B0`);
  h += B(760, 650, 700, 700, `border-radius:50%;background:#A5C79F`);
  // tiny cow
  h += B(842, 636, 56, 30, `background:${col("milk")};border-radius:8px`) + B(852, 642, 14, 10, `background:${col("espresso")};border-radius:4px`) + B(874, 650, 12, 8, `background:${col("espresso")};border-radius:4px`)
     + B(892, 630, 22, 22, `background:${col("milk")};border-radius:6px`) + B(902, 638, 6, 6, `background:${col("espresso")};border-radius:50%`) + B(890, 626, 6, 8, `background:${col("espresso")};border-radius:3px`)
     + B(846, 664, 6, 14, `background:${col("espresso")}`) + B(862, 664, 6, 14, `background:${col("espresso")}`) + B(878, 664, 6, 14, `background:${col("espresso")}`);
  return h;
}
const WP = wallpaper();

function shadow(x, y, w, h, r, layers) {
  return layers.map((l) => B(x - l.grow, y - l.grow + l.dy, w + 2 * l.grow, h + 2 * l.grow, `background:${ESP}${l.a});border-radius:${r + l.grow}px`)).join("");
}
// glass = pre-blurred wallpaper under the panel + milk tint + 1px white border
function glass(x, y, w, h, r, inner = "", { float = true, alpha = TOK.glass.fillAlpha } = {}) {
  const blurred = B(-x, -y, 1024, 768, `filter:blur(${TOK.glass.blurRadius / 2}px)`, WP);
  return (float ? shadow(x, y, w, h, r, TOK.shadow.float) : "") +
    B(x, y, w, h, `border-radius:${r}px;overflow:hidden`, blurred + B(0, 0, w, h, `background:rgba(255,253,248,${alpha})`)) +
    B(x, y, w, h, `border-radius:${r}px;border:1px solid rgba(255,255,255,${TOK.glass.borderAlpha})`, "") +
    B(x, y, w, h, "", inner);
}
const lightsX = (x, y, on = true) => {
  const d = TOK.window.lightD, g = TOK.window.lightGap;
  const cs = on ? [col("straw"), col("honey"), col("matcha")] : [col("latte"), col("latte"), col("latte")];
  return cs.map((c, i) => B(x + i * (d + g), y, d, d, `background:${c};border-radius:50%`)).join("");
};
function win(x, y, w, h, title, body, { focus = true, dark = false } = {}) {
  const t = TOK.window;
  return shadow(x, y, w, h, TOK.radius.window, TOK.shadow.window) +
    B(x, y, w, h, `background:${dark ? col("espresso") : col("milk")};border-radius:${TOK.radius.window}px;border:1px solid ${dark ? "#4A3C34" : col("sand")};overflow:hidden`,
      B(0, 0, w, t.titleH, `background:${dark ? "#4A3C34" : col("cream")};border-bottom:1px solid ${dark ? "#4A3C34" : col("sand")}`) +
      lightsX(t.lightX - 1, t.lightY - 1, focus) +
      TC(0, 8, w, `<b>${title}</b>`, `color:${dark ? col("cream") : focus ? col("espresso") : col("cocoa")}`) + body);
}
function menubar(appName = "Files") {
  const m = TOK.menubar;
  return B(0, 0, 1024, m.h, `background:rgba(255,253,248,${m.fillAlpha});border-bottom:1px solid rgba(255,255,255,.75);overflow:hidden`, B(0, 0, 1024, 768, `filter:blur(${TOK.glass.blurRadius / 2}px)`, WP) + B(0, 0, 1024, m.h, `background:rgba(255,253,248,${m.fillAlpha})`)) +
    drop(12, 8, 13) + T(38, m.textY, `<b>${appName}</b>${"   File   Edit   View   Go   Window   Help"}`) +
    TR(1012, m.textY, "Wi-Fi   87%   Tue 10:41");
}
const dockItems = [["folder", col("honey")], ["term", col("espresso")], ["globe", col("blue")], ["note", col("milk")], ["music", col("straw")], ["photo", col("milk")], ["cal", col("milk")], ["gear", col("latte")]];
function dock(running = [0, 1]) {
  const d = TOK.dock, n = dockItems.length;
  const w = d.pad * 2 + n * d.icon + (n - 1) * d.gap, x = (1024 - w) / 2, y = 768 - d.bottom - d.h;
  let inner = "";
  dockItems.forEach((it, i) => {
    const ix = d.pad + i * (d.icon + d.gap);
    inner += icon(ix, d.pad, d.icon, it[0], it[1], it[0] === "term" ? col("cream") : col("espresso"));
    if (running.includes(i)) inner += B(ix + d.icon / 2 - d.dot / 2, d.h - d.dotGap - d.dot + 1, d.dot, d.dot, `background:${col("cocoa")};border-radius:2px`);
  });
  return glass(x, y, w, d.h, TOK.radius.dock, inner);
}
const toast = (title, sub) => {
  const t = TOK.toast, x = 1024 - t.right - t.w;
  return glass(x, t.top, t.w, t.h, TOK.radius.toast, icon(12, 12, 40, "cal", col("honey") + "88", col("espresso")) + T(64, 12, `<b>${title}</b>`) + T(64, 32, sub, `color:${col("cocoa")}`));
};
const dim = (a) => B(0, 0, 1024, 768, `background:${ESP}${a})`);

/* ------------------------------ windows used in scenes ------------------------------ */
function filesWin(x = 96, y = 64, focus = false) {
  const t = TOK.window, w = 600, h = 400, sw = t.sidebarW;
  let body = B(0, t.titleH, sw, h - t.titleH - 2, `background:${col("oat")};border-right:1px solid ${col("sand")}`);
  body += T(16, 44 + 8, "FAVOURITES", `color:${col("cocoa")}`);
  const fav = ["Documents", "Downloads", "Desktop", "Pictures", "Music"];
  fav.forEach((f, i) => { body += (i === 0 ? B(8, 76 + i * 28, sw - 16, 24, `background:${col("straw")}55;border-radius:${TOK.radius.button}px`) : "") + T(18, 80 + i * 28, f); });
  body += T(16, 232, "DEVICES", `color:${col("cocoa")}`) + T(18, 258, "milk-ssd") + T(18, 286, "Network");
  const files = [["folder", "projects"], ["folder", "school"], ["doc", "todo.txt"], ["photo", "bike.png"], ["note", "ideas.md"], ["folder", "kernel"], ["doc", "notes.txt"], ["term", "run.sh"]];
  files.forEach((f, i) => {
    const cx = sw + 24 + (i % 4) * 100, cy = 52 + Math.floor(i / 4) * 108;
    const isF = f[0] === "folder";
    body += icon(cx + 12, cy, 56, f[0], isF ? col("honey") + "55" : col("cream"), isF ? col("cocoa") : col("espresso"), isF ? "" : `border:1px solid ${col("sand")}`) + TC(cx - 6, cy + 64, 92, f[1]);
  });
  body += B(0, h - t.statusH - 2, w, t.statusH, `background:${col("cream")};border-top:1px solid ${col("sand")}`) + T(sw + 12, h - t.statusH + 2, "8 items, 212 GB available", `color:${col("cocoa")}`);
  return win(x, y, w, h, "Documents", body, { focus });
}
function shellWin(x = 440, y = 270, focus = true) {
  const body = T(14, 46, "andrew@milk ~ % uname -a", `color:${col("cream")}`) + T(14, 64, "milkOS 1.0 aarch64", `color:${col("latte")}`) +
    T(14, 90, "andrew@milk ~ % ls", `color:${col("cream")}`) + T(14, 108, "projects  school  todo.txt  ideas.md", `color:${col("matcha")}`) +
    T(14, 134, "andrew@milk ~ % ", `color:${col("cream")}`) + B(142, 134, 8, 16, `background:${col("cream")}`);
  return win(x, y, 520, 290, "andrew: sh", body, { focus, dark: true });
}

/* ------------------------------ scenes ------------------------------ */
const sceneDesktop = () => WP + menubar() + filesWin() + shellWin() + toast("Dentist", "in 15 minutes") + dock();

function sceneHome() {
  let h = WP + menubar("Home");
  const c = TOK.cards, p = c.pad;
  const card = (x, y, w, hh, label, inner) => glass(x, y, w, hh, c.r, T(p, p, label, `color:${col("cocoa")}`) + inner);
  h += glass(48, 56, 928, 96, c.r, T(p + 8, 16, "Good morning, Andrew", X2 + BD) + T(p + 8, 58, "Tuesday, 20 October   2 events today", `color:${col("cocoa")}`) + TR(928 - 24, 12, "10:41", "font-size:53.333px;line-height:64px;font-weight:700"), { float: true });
  const y1 = 56 + 96 + c.gap;
  h += card(48, y1, 296, 228, "CALENDAR", ["M", "T", "W", "T", "F", "S", "S"].map((d, i) => TC(12 + i * 38, 40, 38, d, `color:${col("cocoa")}`)).join("") +
    Array.from({ length: 21 }, (_, i) => { const n = i + 12, cx = 12 + (i % 7) * 38, cy = 66 + Math.floor(i / 7) * 34, today = n === 20; return (today ? B(cx + 3, cy - 4, 32, 24, `background:${col("straw")};border-radius:12px`) : "") + TC(cx, cy, 38, String(n > 31 ? n - 31 : n), today ? `color:${col("milk")};${BD}` : ""); }).join(""));
  h += card(48 + 296 + c.gap, y1, 296, 228, "NEXT UP", T(p, 44, "2:30 PM", X2 + BD) + T(p, 84, "Dentist", X2) + T(p, 124, "12 Elm St", `color:${col("cocoa")}`) + T(p, 176, "Then: pick up bike, 5:00", `color:${col("cocoa")}`));
  h += card(48 + 2 * (296 + c.gap), y1, 296, 228, "SYSTEM",
    [["cpu", 50, "matcha"], ["mem", 110, "honey"], ["disk", 40, "blue"]].map((m, i) => T(p, 44 + i * 34, m[0]) + B(64, 46 + i * 34, 200, 12, `background:${col("sand")};border-radius:6px`, B(0, 0, m[1], 12, `background:${col(m[2])};border-radius:6px`))).join("") +
    T(p, 156, "Up 2h 14m", `color:${col("cocoa")}`) + T(p, 180, "All systems fine", `color:${col("matcha")};${BD}`));
  const y2 = y1 + 228 + c.gap;
  h += card(48, y2, 456, 188, "NOTES", T(p, 44, "# milkOS 1.0", BD) + T(p, 68, "- ship the compositor") + T(p, 86, "- pick a nicer font") + T(p, 104, "- boot logo, a milk drop") + T(p, 150, "Edited yesterday", `color:${col("latte")}`));
  h += card(48 + 456 + c.gap, y2, 456, 188, "RECENT FILES", [["folder", "projects"], ["doc", "todo.txt"], ["photo", "bike.png"], ["note", "ideas.md"]].map((f, i) => {
    const isF = f[0] === "folder", x = 16 + i * 106;
    return icon(x + 10, 44, 56, f[0], isF ? col("honey") + "55" : col("cream"), isF ? col("cocoa") : col("espresso"), isF ? "" : `border:1px solid ${col("sand")}`) + TC(x - 6, 108, 92, f[1]);
  }).join(""));
  return h + dock([]);
}

function sceneLauncher() {
  const L = TOK.launcher, x = (1024 - L.w) / 2;
  let h = sceneDesktop() + dim(L.dim);
  h += glass(x, L.y, L.w, L.inputH, TOK.radius.launcher, icon(14, 8, L.iconIn, "globe", col("cream"), col("cocoa")) + T(66, 12, "mus", X2) + B(66 + 3 * 16, 12, 3, 32, `background:${col("espresso")}`) + TRi(L.w, L.w - 20, 20, "esc", `color:${col("cocoa")}`), { alpha: 0.7 });
  const ry = L.y + L.inputH + L.gap, rows = [["music", "Music", "Play and organise songs", col("straw")], ["doc", "Musl notes.txt", "~/school", col("cream")], ["gear", "Mute sound", "Settings, Audio", col("cream")]];
  const hh = 16 + 24 + rows.length * L.rowH + 28 + 8;
  let inner = T(16, 12, "APPLICATIONS", `color:${col("cocoa")}`);
  rows.forEach((r, i) => {
    const y = 40 + i * L.rowH;
    inner += (i === 0 ? B(8, y, L.w - 16, L.rowH, `background:${col("straw")}44;border-radius:${TOK.radius.row}px`) : "") +
      icon(16, y + 6, L.iconRow, r[0], r[3], col("espresso"), i ? `border:1px solid ${col("sand")}` : "") + T(64, y + 6, `<b>${r[1]}</b>`) + T(64, y + 26 - 4, r[2], `color:${col("cocoa")}`) + (i === 0 ? TRi(L.w, L.w - 20, y + 16, "Enter", `color:${col("cocoa")}`) : "");
  });
  inner += T(16, 40 + rows.length * L.rowH + 8, "Tab for actions   &gt; for commands   ? for help", `color:${col("cocoa")}`);
  h += glass(x, ry, L.w, hh + 8, TOK.radius.launcher, inner, { alpha: 0.7 });
  return h;
}

function sceneSpaces() {
  const S = TOK.spaces;
  let h = WP + dim(S.dim) + menubar("Overview");
  const sp = [["1 Code", 1], ["2 Web", 0], ["3 Notes", 0], ["+", 0]];
  const sx = (1024 - (sp.length * S.thumbW + (sp.length - 1) * S.thumbGap)) / 2;
  sp.forEach((s, i) => { const x = sx + i * (S.thumbW + S.thumbGap); h += glass(x, S.stripY, S.thumbW, S.thumbH, S.thumbR, B(8, 8, 50, 34, `background:${col("cream")};border-radius:4px`) + B(64, 18, 52, 46, `background:${col("cream")};border-radius:4px`) + TC(0, 64, S.thumbW, s[0], `color:${col("cocoa")}`), { alpha: s[1] ? 0.85 : 0.5 }) + (s[1] ? B(x, S.stripY, S.thumbW, S.thumbH, `border:${S.selBorder}px solid ${col("honey")};border-radius:${S.thumbR}px`) : ""); });
  const cell = (cx, cy, w, hh, title, body, sel) => shadow(cx, cy, w, hh, TOK.radius.window, TOK.shadow.float) +
    B(cx, cy, w, hh, `background:${col("milk")};border-radius:${TOK.radius.window}px;border:${sel ? S.selBorder : 1}px solid ${sel ? col("honey") : col("sand")};overflow:hidden`,
      B(0, 0, w, 26, `background:${col("cream")};border-bottom:1px solid ${col("sand")}`) + lightsX(10, 7, true).replace(/width:12px;height:12px/g, "width:12px;height:12px") + body) + TC(cx, cy + hh + 8, w, title, `color:${col("milk")}`);
  h += cell(60, 170, 300, 210, "Files", icon(20, 44, 40, "folder", col("honey") + "55", col("cocoa")) + icon(80, 44, 40, "folder", col("honey") + "55", col("cocoa")) + icon(140, 44, 40, "doc", col("cream"), col("espresso")), false);
  h += cell(390, 170, 300, 210, "Shell", B(0, 26, 300, 184, `background:${col("espresso")}`) + T(10, 40, "$ cargo build", `color:${col("cream")}`) + T(10, 58, "Finished in 2.1s", `color:${col("matcha")}`) + T(10, 76, "$ _", `color:${col("cream")}`), true);
  h += cell(720, 170, 250, 210, "Notes", T(14, 40, "# ideas") + T(14, 58, "- milk drop"), false);
  h += cell(60, 440, 300, 210, "Web", B(0, 26, 300, 28, `background:${col("oat")}`) + T(10, 32, "milk.example", `color:${col("cocoa")}`) + T(14, 70, "Welcome", X2), false);
  h += cell(390, 440, 300, 210, "Music", icon(20, 44, 80, "music", col("straw"), col("espresso")) + T(116, 56, "Cream Soda Skies") + T(116, 76, "Lactose Intolerants", `color:${col("cocoa")}`), false);
  h += cell(720, 440, 250, 210, "Settings", T(14, 40, "Display") + T(14, 64, "Sound") + T(14, 88, "Network"), false);
  return h + TC(0, 722, 1024, "Click a window to focus   Drag a window to another space   Esc to leave", `color:${col("milk")}`);
}

function sceneControl() {
  const c = TOK.control, x = 1024 - c.right - c.w;
  const tile = (tx, ty, tw, label, sub, on) => B(tx, ty, tw, c.tileH, `background:${on ? col("straw") : "rgba(255,253,248,.7)"};border-radius:16px`) + T(tx + 14, ty + 10, `<b>${label}</b>`, on ? `color:${col("milk")}` : "") + T(tx + 14, ty + 30, sub, on ? `color:${col("milk")}` : `color:${col("cocoa")}`);
  const slider = (sy, label, v) => B(16, sy, c.w - 32, c.tileH, "background:rgba(255,253,248,.7);border-radius:16px") + T(30, sy + 8, `<b>${label}</b>`) + B(30, sy + 36, c.w - 60, c.sliderH, "background:rgba(122,102,82,.25);border-radius:4px", B(0, 0, (c.w - 60) * v, c.sliderH, `background:${col("espresso")};border-radius:4px`));
  const inner = tile(16, 16, 124, "Wi-Fi", "home-5G", false) + tile(148, 16, 124, "Focus", "On", true) + slider(88, "Display", 0.68) + slider(160, "Sound", 0.4) + T(18, 236, "Battery 87%   2h 14m", `color:${col("cocoa")}`) + T(18, 258, "Tue 20 October", `color:${col("cocoa")}`);
  return WP + menubar() + filesWin(96, 64, false) + glass(x, c.top, c.w, c.h, TOK.radius.card, inner) + dock();
}

function sceneRedlines() {
  let h = sceneDesktop();
  const ln = (x, y, w, hh, c = "#D6334A") => B(x, y, w, hh, `border:1px dashed ${c}`);
  const lab = (x, y, t) => T(x, y, `&nbsp;${t}&nbsp;`, `background:#D6334A;color:#fff;border-radius:4px`);
  const m = TOK.menubar, d = TOK.dock, n = dockItems.length, dw = d.pad * 2 + n * d.icon + (n - 1) * d.gap, dx = (1024 - dw) / 2, dy = 768 - d.bottom - d.h, t = TOK.window;
  h += B(0, 0, 1024, 768, "background:rgba(255,253,248,.0)");
  h += ln(0, 0, 1024, m.h) + lab(300, 6, `menu bar h${m.h}, α${m.fillAlpha}, text y+${m.textY}`);
  h += ln(dx, dy, dw, d.h) + lab(dx, dy - 22, `dock h${d.h}  r${TOK.radius.dock}  pad ${d.pad}  icon ${d.icon}  gap ${d.gap}`);
  h += ln(dx, dy + d.h, dw, d.bottom) + lab(dx + dw + 8, dy + d.h - 6, `${d.bottom}px from bottom`);
  h += ln(96, 64, 600, 400) + lab(300, 44, `window r${TOK.radius.window}, 1px sand border`);
  h += ln(96, 64, 600, t.titleH) + lab(560, 72, `title bar h${t.titleH}`);
  h += ln(96 + t.lightX - 1, 64 + t.lightY - 1, 3 * t.lightD + 2 * t.lightGap, t.lightD) + lab(96, 44, `lights d${t.lightD} gap${t.lightGap} @${t.lightX},${t.lightY}`);
  h += ln(96, 64 + t.titleH, t.sidebarW, 366) + lab(100, 64 + t.titleH + 170, `sidebar w${t.sidebarW}`);
  const tx = 1024 - TOK.toast.right - TOK.toast.w;
  h += ln(tx, TOK.toast.top, TOK.toast.w, TOK.toast.h) + lab(tx - 30, TOK.toast.top + TOK.toast.h + 4, `toast ${TOK.toast.w}x${TOK.toast.h} r${TOK.radius.toast}`);
  h += lab(440, 556, "window shadow: 3 stacked rounded rects (see SPEC)");
  return h;
}

export const SLIDES = [
  { id: "desktop", name: "Desktop", draw: sceneDesktop, from: "01 + 10", note: "Menu bar, dock and toast are glass over the wallpaper (pre-blurred copy). Windows are opaque with 12px corners and stacked-rect shadows. Focused window has coloured lights, the other greys out." },
  { id: "home", name: "Home (Today)", draw: sceneHome, from: "06 + 10", note: "No windows open: the wallpaper becomes a glanceable home. Cards are the same glass panels. Same dock, so the shell never changes shape." },
  { id: "launcher", name: "Launcher", draw: sceneLauncher, from: "03", note: "One key, type, Enter. Dims what is behind by 25%. Results show apps, files and commands in one list." },
  { id: "spaces", name: "Spaces", draw: sceneSpaces, from: "07", note: "Scaled blits of each window's offscreen buffer (needs the per-window buffers), plus a strip of virtual desktops. Focused window gets a 4px honey border." },
  { id: "control", name: "Control centre", draw: sceneControl, from: "10", note: "Quick settings panel from the menu bar. Glass again, so it costs one more pre-blurred-wallpaper sample, no live blur." },
  { id: "redlines", name: "Redlines", draw: sceneRedlines, from: "spec", note: "The same desktop with the key measurements marked. Full numbers are in SPEC.md." },
];


/* ------------------------------ dark theme + contrast (used by SPEC.md) ------------------------------ */
export const DARK = { milk: "#2E2520", cream: "#382D27", oat: "#42352D", sand: "#55463C", latte: "#8C7764", cocoa: "#BEA890", espresso: "#F4EBE0", white: "#7A6658" };
export const FIXED = { "Terminal body": "#3A2E29", "Terminal text": "#FBF6ED", "Ink (shadows, dims, glyphs on honey/straw/blue tiles)": "#3A2E27", "Terminal bar and border": "#4A3C34" };
const lin = (v) => { v /= 255; return v <= 0.04045 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4); };
const lumOf = (h) => { const [r, g, b] = [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16)); return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b); };
const ratio = (a, b) => { const [x, y] = [lumOf(a), lumOf(b)].sort((p, q) => q - p); return (x + 0.05) / (y + 0.05); };
const mixHex = (a, b, t) => "#" + [1, 3, 5].map((i) => Math.round(parseInt(a.slice(i, i + 2), 16) * (1 - t) + parseInt(b.slice(i, i + 2), 16) * t).toString(16).padStart(2, "0")).join("");
const LIGHT = Object.fromEntries(Object.entries(TOK.color).map(([k, v]) => [k, v[0]]));
const SEL = 0.27; // selected row = straw at 27% over the surface
const pairRows = (T, theme) => {
  const t = { milk: T.milk, cream: T.cream, oat: T.oat, sand: T.sand, text: theme === "dark" ? DARK.espresso : LIGHT.espresso, sub: theme === "dark" ? DARK.cocoa : LIGHT.cocoa, off: T.latte };
  const sel = mixHex(t.oat, LIGHT.straw, SEL);
  const rows = [
    ["Primary text on window body", t.text, t.milk, 4.5], ["Primary text on title bar", t.text, t.cream, 4.5], ["Primary text on sidebar", t.text, t.oat, 4.5],
    ["Secondary text on window body", t.sub, t.milk, 4.5], ["Secondary text on title bar", t.sub, t.cream, 4.5], ["Secondary text on sidebar", t.sub, t.oat, 4.5],
    ["Primary text on a selected sidebar row", t.text, sel, 4.5], ["Secondary text on a selected sidebar row (not allowed)", t.sub, sel, 4.5],
    ["Tab close x, resting (secondary colour) on the title bar", t.sub, t.cream, 3],
    ["Tab close x on hover (primary) on its hover circle (sand)", t.text, t.sand, 3],
    ["Disabled text on window body (exempt from AA)", t.off, t.milk, 0],
  ];
  return rows.map(([name, fg, bg, min]) => { const r = ratio(fg, bg); return `| ${name} | ${fg} on ${bg} | ${r.toFixed(2)}:1 | ${min ? (r >= min ? "pass" : "FAILS " + min + ":1") : "n/a"} |`; }).join("\n");
};
const termRows = [["Terminal text on Terminal body", FIXED["Terminal text"], FIXED["Terminal body"]], ["Terminal tab, inactive (dark cocoa) on Terminal bar", DARK.cocoa, FIXED["Terminal bar and border"]], ["Terminal tab, active text on Terminal body", FIXED["Terminal text"], FIXED["Terminal body"]]]
  .map(([n, f, b]) => `| ${n} | ${f} on ${b} | ${ratio(f, b).toFixed(2)}:1 | ${ratio(f, b) >= 4.5 ? "pass" : "FAILS"} |`).join("\n");

/* ------------------------------ page ------------------------------ */
const N = SLIDES.length;
const idn = (i) => `s${((i % N) + N) % N}`;
const svg = (s, cls = "") => `<svg class="scene ${cls}" viewBox="0 0 1024 768" role="img" aria-label="milkOS 1.0 mockup: ${s.name}"><foreignObject x="0" y="0" width="1024" height="768"><div class="stage">${s.draw()}</div></foreignObject></svg>`;
const rules = SLIDES.map((s, i) => `#r-${idn(i)}:checked ~ .wrap .p-${idn(i)}{display:grid}
#r-${idn(i)}:checked ~ .wrap .n-${idn(i)}{display:flex}
#r-${idn(i)}:checked ~ .wrap .t-${idn(i)}{border-color:var(--accent)}`).join("\n");

const page = `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>milkOS 1.0 Mockup</title>
<style>
:root{--bg:#FBF6EC;--card:#FFFDF8;--line:#E2D5BD;--ink:#3A2E28;--mute:#7A6652;--accent:#C9684F}
@media (prefers-color-scheme:dark){:root{--bg:#1E1915;--card:#2A231D;--line:#443A30;--ink:#F3EBDD;--mute:#B9A68C;--accent:#E9966F}}
*{box-sizing:border-box}html,body{margin:0}
body{background:var(--bg);color:var(--ink);font:16px/1.5 ui-sans-serif,system-ui,-apple-system,"Segoe UI",sans-serif}
.r{position:absolute;opacity:0;pointer-events:none}
.wrap{max-width:1300px;margin:0 auto;padding:20px 16px 48px}
header{display:flex;flex-wrap:wrap;gap:12px;align-items:baseline;justify-content:space-between;margin-bottom:16px}
h1{font-size:22px;margin:0}.sub{color:var(--mute);font-size:14px;max-width:760px}
.nav{display:none;gap:8px;align-items:center}
.btn{display:inline-block;color:var(--ink);background:var(--card);border:1px solid var(--line);border-radius:10px;padding:8px 16px;cursor:pointer;min-height:44px;line-height:26px;user-select:none}
.btn:hover{border-color:var(--accent)}.btn:focus-visible,.thumb:focus-visible{outline:3px solid var(--accent);outline-offset:2px}
.count{min-width:64px;text-align:center;color:var(--mute)}
.panel{display:none;grid-template-columns:minmax(0,1fr) 300px;gap:20px;align-items:start}
@media(max-width:980px){.panel{grid-template-columns:1fr}}
.stagebox{background:var(--card);border:1px solid var(--line);border-radius:14px;padding:12px}
.stagewrap{border-radius:6px;border:1px solid var(--line);overflow:hidden;line-height:0}
.scene{display:block;width:100%;height:auto}
.stage{width:1024px;height:768px;position:relative;overflow:hidden;font:13.333px/16px "Unifont","Menlo","SF Mono",ui-monospace,monospace;color:#3A2E28}
.stage div{box-sizing:border-box}
.notes{background:var(--card);border:1px solid var(--line);border-radius:14px;padding:18px}
.notes h2{margin:0 0 2px;font-size:20px}.notes .tag{color:var(--mute);margin:0 0 12px;font-size:14px}
.strip{display:grid;grid-template-columns:repeat(6,minmax(0,1fr));gap:10px;margin-top:20px}
@media(max-width:700px){.strip{grid-template-columns:repeat(2,minmax(0,1fr))}}
.thumb{display:block;cursor:pointer;border:2px solid var(--line);border-radius:10px;background:var(--card);padding:6px}
.thumb .tw{border-radius:4px;overflow:hidden;line-height:0}.thumb .tl{font-size:13px;margin-top:5px;padding:0 2px}
${rules}
</style></head><body>
${SLIDES.map((s, i) => `<input class="r" type="radio" name="s" id="r-${idn(i)}"${i === 0 ? " checked" : ""}>`).join("")}
<div class="wrap">
<header><div><h1>milkOS 1.0: the merged look</h1><div class="sub">Concepts 01 Whole Milk + 03 Skim + 06 Today + 07 Spaces + 10 Glass, as one system. Glass is cheap: panels sample a wallpaper that was blurred once, never live. Numbers are in SPEC.md.</div></div>
${SLIDES.map((s, i) => `<div class="nav n-${idn(i)}"><label class="btn" for="r-${idn(i - 1)}" role="button" tabindex="0" aria-label="Previous">&larr; Prev</label><span class="count">${i + 1} / ${N}</span><label class="btn" for="r-${idn(i + 1)}" role="button" tabindex="0" aria-label="Next">Next &rarr;</label></div>`).join("")}
</header>
${SLIDES.map((s, i) => `<article class="panel p-${idn(i)}"><div class="stagebox"><div class="stagewrap">${svg(s)}</div></div><aside class="notes"><h2>${s.name}</h2><p class="tag">from ${s.from}</p><p>${s.note}</p></aside></article>`).join("")}
<div class="strip">${SLIDES.map((s, i) => `<label class="thumb t-${idn(i)}" for="r-${idn(i)}" tabindex="0" role="button" aria-label="Show ${s.name}"><div class="tw">${svg(s)}</div><div class="tl"><b>${s.name}</b></div></label>`).join("")}</div>
</div>
<script>addEventListener('keydown',function(e){var r=[].slice.call(document.querySelectorAll('.r')),c=r.findIndex(function(x){return x.checked});if(e.key==='ArrowRight')r[(c+1)%r.length].checked=true;else if(e.key==='ArrowLeft')r[(c-1+r.length)%r.length].checked=true;else if((e.key==='Enter'||e.key===' ')&&e.target.tagName==='LABEL'){e.preventDefault();e.target.click()}})</script>
</body></html>`;

/* ------------------------------ SPEC.md (generated from TOK) ------------------------------ */
const sh = (arr) => arr.map((l, i) => `  ${i + 1}. grow ${l.grow}px, move down ${l.dy}px, alpha ${l.a}`).join("\n");
const spec = `# milkOS 1.0 UI spec

For the engineer building the compositor and shell. Generated from \`v1.mjs\` (the mockup uses the same numbers), so if you change a value there, regenerate. Mockup: \`milkos-1.0.html\`. App specs (Activity Monitor, Text Editor, Calendar, Clock/Timer, Preview): \`SPEC-APPS.md\`. Art direction: \`ART.md\`. Screen: ${TOK.screen.w}x${TOK.screen.h}, 32-bit. Layout snaps to a ${TOK.screen.grid}px grid.

## Colour tokens (sRGB)
| Token | Hex | Use |
|---|---|---|
${Object.entries(TOK.color).map(([k, v]) => `| ${k} | ${v[0]} | ${v[1]} |`).join("\n")}

Alpha tints used: straw at 0x44 (27%) for selection rows, straw at 0x55 (33%) for sidebar selection, honey at 0x55 for folder tiles, espresso for shadows (below).

## Type
- ${TOK.text.ui}
- Bold: ${TOK.text.bold}
- Heading: ${TOK.text.heading}
- Display: ${TOK.text.display}
- ${TOK.text.note}

## Corner radii (px)
${Object.entries(TOK.radius).map(([k, v]) => `- ${k}: ${v}`).join("\n")}

## Glass (the cheap version)
- Keep two copies of the wallpaper: \`W\` (sharp) and \`Wb\` (box blur radius ${TOK.glass.blurRadius}, ${TOK.glass.blurPasses} passes). Rebuild \`Wb\` only when the wallpaper changes.
- A glass panel = copy the \`Wb\` pixels under its rect, blend milk (#FFFDF8) at alpha ${TOK.glass.fillAlpha} (launcher and control surfaces: 0.7), then a 1px inner border, white at alpha ${TOK.glass.borderAlpha}.
- ${TOK.glass.rule}
- Live blur over windows is not supported. If a glass panel ever overlaps a window, draw it as plain milk at alpha 0.92.

## Shadows
Colour ${TOK.shadow.color}. ${TOK.shadow.how}
- Window (focused or not):
${sh(TOK.shadow.window)}
- Floating panels (dock, toast, launcher, control centre, cards):
${sh(TOK.shadow.float)}

## Menu bar
Height ${TOK.menubar.h}. Glass, fill alpha ${TOK.menubar.fillAlpha}, 1px white bottom edge. Left: logo drop 13x13 at x12, y8, then app name (bold) at x38, menu items separated by ${TOK.menubar.itemGap}px (3 spaces of 8px). Right: status text right-aligned, ${TOK.menubar.padX}px from edge. Text top at y${TOK.menubar.textY}.

## Dock
Height ${TOK.dock.h}, radius ${TOK.radius.dock}, glass. Padding ${TOK.dock.pad}, icons ${TOK.dock.icon}x${TOK.dock.icon}, gap ${TOK.dock.gap}. Width = ${TOK.dock.pad * 2} + n*${TOK.dock.icon} + (n-1)*${TOK.dock.gap}. Centred horizontally, ${TOK.dock.bottom}px above the bottom edge. Running app: ${TOK.dock.dot}x${TOK.dock.dot} dot in cocoa, ${TOK.dock.dotGap}px above the dock's bottom edge, centred under the icon. Icon tile radius is 24% of its size.

## Window chrome
- Radius ${TOK.radius.window}, 1px sand border (terminal: 1px #4A3C34), opaque body (milk; terminal espresso).
- Title bar height ${TOK.window.titleH}, fill cream (terminal #4A3C34), 1px sand line under it. Title text bold, horizontally centred in the bar, top at y8.
- Traffic lights: ${TOK.window.lightD}px circles, ${TOK.window.lightGap}px gap, first light's top-left at (${TOK.window.lightX},${TOK.window.lightY}) inside the window (border included). Order: close straw, minimise honey, zoom matcha. Unfocused window: all three latte, title text cocoa.
- Minimum size ${TOK.window.min[0]}x${TOK.window.min[1]}. Body padding ${TOK.window.bodyPad}. Files sidebar width ${TOK.window.sidebarW} (oat, 1px sand right edge). Status bar height ${TOK.window.statusH} (cream, 1px sand top edge).

## Tabs (Terminal tabs and Tabs-everywhere window tabs share this)
One look for every tab strip, so a Terminal's tabs and a Files window that has absorbed a Terminal look the same. Added 2026-10-21 at the engineer's request.
- Strip: a window with two or more tabs uses a ${TOK.tabs.stripH}px bar in place of the ${TOK.window.titleH}px title bar. Same fill, 1px sand line under it. The traffic lights keep their x (${TOK.window.lightX}) and sit at y${TOK.window.lightY + 2}, centred in the taller bar.
- Tab: height ${TOK.tabs.tabH}, top y${TOK.tabs.tabY} inside the strip (so it touches the line under the strip), width between ${TOK.tabs.minW} and ${TOK.tabs.maxW} (tabs share the strip equally, never narrower than ${TOK.tabs.minW}), ${TOK.tabs.gap}px between tabs. The first tab starts at x${TOK.tabs.firstX}. Corner radius ${TOK.tabs.radiusTop}px on the top corners only, square at the bottom. Padding ${TOK.tabs.padL}px left, ${TOK.tabs.padR}px right.
- Active tab: fill is the window body colour (milk), 1px sand border on the top, left and right but not the bottom, so it joins the content. Title in primary text, bold.
- Inactive tab: no fill, no border, title in secondary text. Hover: oat at 60% fill.
- Title: one line, cut with "~" to fit (about 13 characters in the default ${TOK.tabs.minW + 48}px tab). Terminal tabs read "1 msh", "2 msh".
- Close x: the letter x in the secondary colour of the tab it sits on (a light tab: secondary text; a Terminal-coloured tab: #BEA890 in any window, so a Terminal tab merged into a light window must not use the light cocoa, which is only 1.94:1 on the Terminal fill), right-aligned with ${TOK.tabs.padR}px right padding, hit target ${TOK.tabs.closeHit}x${TOK.tabs.closeHit}. Shown on every tab at rest (not only on hover). Hover: a ${TOK.tabs.closeHit}px circle in sand appears behind it and the x turns primary text. Pressing it closes the tab; closing the last tab closes the window.
- New tab (+): ${TOK.tabs.plusW}x${TOK.tabs.plusH}, radius ${TOK.tabs.plusR}, oat fill, 4px after the last tab, the "+" centred.
- Terminal tabs: same geometry with Terminal colours. Bar and border #4A3C34, active tab fill is the Terminal body #3A2E29 with Terminal text #FBF6ED, inactive title and close x in the dark cocoa #BEA890 (4.63:1 on the bar; the lighter latte would be only 2.48:1), hover circle #55463C. These colours are the same in the light and dark themes.
- Dragging a tab or a window onto a strip: the dragged item is lifted with the floating shadow. The drop target on the strip is a rounded rect where the new tab would go, straw at 20% fill, ${TOK.tabs.dropBorder.replace("straw", "straw")} border, radius ${TOK.tabs.radiusTop}, with "Drop to add as a tab" in primary text, bold (straw text on the straw tint is only 1.96:1; the dashed straw border carries the colour), and at least 176px wide so the label fits. The dragged window ghost is at 92% opacity with a 2px straw border. Dragging a tab out of its strip makes it a window again.
- Keys: Cmd-T new tab, Cmd-W close tab, Ctrl-Tab and Ctrl-Shift-Tab next and previous, Cmd-1 to Cmd-9 jump. The first build kept the existing Terminal keys Cmd-T and Cmd-W, so the spec now matches it. Ctrl may be accepted as an alias where it is free.
- Notifications attached to tabs (a dot on the strip) are parked for later and not part of this spec.

## Launcher (Skim)
Width ${TOK.launcher.w}, centred, top at y${TOK.launcher.y}. Input row height ${TOK.launcher.inputH}, radius ${TOK.radius.launcher}, glass alpha 0.7, query at 2x text, leading icon ${TOK.launcher.iconIn}. Results panel ${TOK.launcher.gap}px below, same radius. Result row height ${TOK.launcher.rowH}, radius ${TOK.radius.row}, selected row straw at 27%, icon ${TOK.launcher.iconRow}. Max ${TOK.launcher.maxRows} rows. Dims the screen behind with espresso at alpha ${TOK.launcher.dim}.

## Toast / notification
${TOK.toast.w}x${TOK.toast.h}, radius ${TOK.radius.toast}, glass, ${TOK.toast.right}px from the right edge, ${TOK.toast.top}px from the top. Icon 40x40 at (12,12), title bold at (64,12), subtitle cocoa at (64,32).

## Notification drawer (side drawer)
Chosen look (Andrew, 2026-10-21). Panel ${TOK.drawer.w} wide, ${TOK.drawer.right}px from the right edge, from y${TOK.drawer.top} down to ${TOK.drawer.bottom}px above the bottom edge. It is drawn above the dock, so it covers the dock's right end while open. Radius ${TOK.radius.launcher}, glass at alpha ${TOK.drawer.alpha}, floating shadow. Inner padding ${TOK.drawer.pad}.
- Header (${TOK.drawer.headerH}px): "Notifications" at the top left, "Clear all" right-aligned in cocoa, then filter chips (All, Mentions, Today), ${TOK.drawer.chipH}px high, fully rounded, selected chip espresso with cream text.
- Group label: 8x16 uppercase cocoa ("MESSAGES"), count right-aligned. Groups collapse and expand.
- Card: width drawer minus ${TOK.drawer.pad * 2}, height ${TOK.drawer.cardH} (compact ${TOK.drawer.cardCompactH}), radius ${TOK.drawer.cardR}, milk at 82% with a 1px white edge. App icon ${TOK.drawer.iconIn}x${TOK.drawer.iconIn} at (10,10); title bold at (56,8); time right-aligned in cocoa at the same line; one line of body in cocoa at (56,28), cut with "~" when too long (use a real ellipsis, U+2026, if the font draws it). Selected card: straw at 13% fill and a 1px straw border.
- Today card: label, then up to three events (time bold, title). Focus row (the label is "Focus", not "Do Not Disturb": it is the same single setting as the Control centre Focus tile, with the sub-line "On until 12:00" while on): ${TOK.drawer.dndH}px high at the bottom with a switch (44x24, straw when on).
- Opening: click the menu bar bell, or Ctrl-N. Closing: Esc, or click outside. Snap open (no animation required).
- Keys: Up/Down select, Return opens the source app or window, Backspace dismisses the selected card, Left/Right collapse or expand a group, Tab cycles the filter chips.
- Arrival: a toast (above) shows for ${TOK.drawer.toastSecs} seconds, pauses while hovered, and clicking it opens the drawer on that item. If the drawer is open, the new card appears at the top of its group instead.
- Badges: the menu bar bell shows the total unread count; dock icons show their own app's count (straw circle, 18px, milk 2px ring). Focus holds toasts and sounds but still collects cards, and shows a moon on the bell.

## Control centre
${TOK.control.w}x${TOK.control.h}, radius ${TOK.radius.card}, glass, ${TOK.control.right}px from right, ${TOK.control.top}px from top. Tiles/sliders ${TOK.control.tileH} high, radius 16, white at 70%. Slider track ${TOK.control.sliderH} high, fill espresso.

## Home (Today) cards
Radius ${TOK.cards.r}, padding ${TOK.cards.pad}, gap ${TOK.cards.gap}, ${TOK.cards.cols}-column grid inside 48px side margins. Labels are 8x16 uppercase cocoa.

## Spaces (overview)
Dim the wallpaper with espresso at ${TOK.spaces.dim}. Space strip at y${TOK.spaces.stripY}: thumbnails ${TOK.spaces.thumbW}x${TOK.spaces.thumbH}, gap ${TOK.spaces.thumbGap}, radius ${TOK.spaces.thumbR}; the current space has a ${TOK.spaces.selBorder}px honey border. Window cells are each window's offscreen buffer scaled ${TOK.spaces.windowScale}, with the title under it in milk. Focused window: ${TOK.spaces.selBorder}px honey border.

## Wallpaper
${TOK.wallpaper}

## Dark theme ("Chocolate milk")
Values from kernel/src/wm/theme.rs tc() as given by the engineer, 2026-10-21. Every light neutral token maps to a dark one. The accents (straw, honey, matcha, blue, lav) are unchanged in both themes.
| Token | Light | Dark | Use |
|---|---|---|---|
${["milk","cream","oat","sand","latte","cocoa","espresso"].map((k) => `| ${k} | ${LIGHT[k]} | ${DARK[k]} | ${TOK.color[k][1]} |`).join("\n")}
| white | #FFFFFF | ${DARK.white} | glass edges, hover lightening |

Fixed in both themes (deliberately one step off the tokens above so the dark map skips them):
${Object.entries(FIXED).map(([k, v]) => `- ${k}: ${v}`).join("\n")}

- Glass in dark: re-tinted from the dark milk, alphas unchanged (panels ${TOK.glass.fillAlpha}, launcher and control surfaces 0.7, border ${TOK.glass.borderAlpha}).
- Wallpaper: "Night pasture" (moon, stars, sleeping cow). Settings offers Auto, Dawn, Night and Oat.

### Contrast, measured from these exact values (WCAG AA: 4.5:1 for text, 3:1 for icons)
Light theme:
| Pair | Colours | Ratio | AA |
|---|---|---|---|
${pairRows(LIGHT, "light")}

Dark theme:
| Pair | Colours | Ratio | AA |
|---|---|---|---|
${pairRows(DARK, "dark")}

Terminal (same in both themes):
| Pair | Colours | Ratio | AA |
|---|---|---|---|
${termRows}

Rules that come out of the numbers:
- **Secondary text never goes on a selected row.** A selected row is straw at ${SEL * 100}% over the surface. Primary text on it is fine in both themes; secondary text on it is only about 3.3:1 in dark, so use primary there.
- **cocoa changed (light):** #7A6652 was 4.40:1 on the sidebar, just under 4.5. It is now #6B5846 (5.45:1 on the sidebar). The dark cocoa #BEA890 is unchanged. Anything hard-coded to #7A6652 should move to the token.
- **Disabled text (latte) is exempt** from the 4.5:1 rule but should not carry meaning on its own. The tab close x uses cocoa, not latte, for that reason.
- **Dimming an inactive window must not drop its text below 4.5:1.** In the dark theme the shipped inactive Terminal text is #8C7764 on #3A2E29, 3.08:1 (the light theme uses #C4AE8F on the same body, 6.11:1). Use #BEA890 (5.74:1) for dimmed Terminal text, or dim the window with a veil instead of recolouring the text.
- **Hairlines** (sand) are decorative dividers, about 1.3:1 to 1.7:1 in dark. Where a border is the only thing showing a control's edge (inputs, buttons), add a fill or use a stronger edge.

## Buttons
Added 2026-10-21 after a contrast check of the shipped screens: filled buttons were cream text on straw, 2.42:1. The fix keeps the pink and darkens the text. The text colour is the fixed ink (${TOK.button.ink}), not espresso, because espresso turns light in the dark theme.
| Kind | Fill | Text | Contrast |
|---|---|---|---|
| Primary (filled) | straw ${LIGHT.straw} | ink ${TOK.button.ink}, bold | ${ratio(TOK.button.ink, LIGHT.straw).toFixed(2)}:1 |
| Primary, hover | ${TOK.button.hover} | ink | ${ratio(TOK.button.ink, TOK.button.hover).toFixed(2)}:1 |
| Primary, pressed | ${TOK.button.pressed} | ink | ${ratio(TOK.button.ink, TOK.button.pressed).toFixed(2)}:1 |
| Secondary | oat, 1px sand border | primary text (espresso; #F4EBE0 in dark) | ${ratio(LIGHT.espresso, LIGHT.oat).toFixed(2)}:1 light, ${ratio(DARK.espresso, DARK.oat).toFixed(2)}:1 dark |
| Danger | ${TOK.button.danger} | #FFFDF8 (fixed, not the milk token) | ${ratio("#FFFDF8", TOK.button.danger).toFixed(2)}:1 |
| Disabled | oat, 1px sand border | latte (exempt from 4.5:1, never the only signal) | ${ratio(LIGHT.latte, LIGHT.oat).toFixed(2)}:1 |
- Sizes: pill (inside cards, toasts, banners) height ${TOK.button.pillH}, fully rounded (radius ${TOK.button.pillR}), padding ${TOK.button.padX} each side. Dialog button (sheets, panels, Open/Save) height ${TOK.button.dialogH}, radius ${TOK.button.dialogR}, minimum width ${TOK.button.minW}. Text is bold (the faux bold).
- Button row in a sheet: right-aligned, 8px apart, the primary on the far right, the cancel or secondary action to its left. A destructive primary uses the Danger kind.
- Focus ring: 2px honey, 2px outside the button. Pressed buttons move nothing.
- Where it applies: every filled straw pill (Welcome tour buttons, Restore this version, Share, Send), dialog buttons (Open, Cancel), and sheets. Straw is still used freely for fills without text on them (selection, progress, the traffic light).

## Files window: toolbar, sidebar and status bar
Proposal from the 2026-10 screen review. Today the window has only a sidebar, a grid and a status bar with an "up" pill squeezed into the corner.
- Toolbar: ${TOK.files.toolbarH}px high, directly under the title bar, cream, 1px sand line below. Left to right at 12px padding: back and forward (${TOK.files.navD}px circles, oat; forward is latte when disabled), the path bar (${TOK.files.pathH}px high, radius ${TOK.files.pathH / 2}, milk with 1px sand, "Home > Documents", each part clickable, replaces the old "up" pill), search (${TOK.files.searchW}px wide, same style), the Share button (a secondary pill; primary while a file is selected), and a view menu button (list or grid).
- Sidebar (${TOK.files.sidebarW} wide): FAVOURITES (Home, Documents, Downloads, Desktop, Pictures, Music), NEARBY (Milk Drop devices found on the link: a dot, green when the device is receiving and latte when not; dropping a file on a name sends it), SHARED (the Mac folder). "milk" and "Milk" are not good labels (they differ by a capital): name them "This machine" and "Milk disk". Time Machine is not a device: give it a clock button in the toolbar (it opens the versions view) and keep the "Browse All Versions" menu item.
- Status bar: ${TOK.files.statusH} high. Left to right: "1 of 4 selected, 1.0 MB", then "212 GB available" on the right. Path text uses secondary text, never latte (latte on cream is 1.99:1).
- Empty folder: a centred line in secondary text, "This folder is empty", and a faint dashed outline where dropped files would go.

## Menu bar status area
The right side of the menu bar today shows only the CPU meter and the clock, so there is nowhere to open the notification drawer from and no sign of Focus, network or sound. Right to left from the clock, ${TOK.menuStatus.gap}px apart, each a ${TOK.menuStatus.glyph}px glyph with a ${TOK.menuStatus.hitH}px high hit target:
1. Clock (opens Today).
2. CPU meter (opens Activity).
3. Volume (opens the Control centre).
4. Network (opens the Control centre).
5. Focus moon, shown only while Focus is on.
6. Bell with the unread count (opens the notification drawer). This is the one that must exist before the drawer ships.
Icons are primary text colour, 1px lines on the 8x16 grid look; counts use the 18px straw badge from the Notification drawer section.

## Milk Drop sheet (Files > Share)
Mockups: share.html. Built on the existing \`drop\` commands only (docs/networking.md), so no new kernel work.
- Sending: the Share button (or dropping a file on a NEARBY name) opens a sheet, ${520}px wide, radius 14, window shadow, header 44 (title "Milk Drop", Esc hint on the right), below the Files toolbar's title bar.
- Device rows: 64 high, radius 12, 12px side margin, 12px between. Name bold, fingerprint in four groups of four (e.g. 3fa2 91bc 07de 44a1) in secondary text, a trust chip on the right (Known: matcha at 27%, New device: honey at 27%, KEY CHANGED: ${TOK.button.danger} with light text, Not receiving: latte and the row at 55% opacity with "open Milk Drop there"). Selected row: straw 20% fill and a 2px straw border.
- Data: \`MILK_TABLE=1 drop list\` (name, address, fingerprint, receiving, trust). Rescan while the sheet is open (about every 3 seconds; one scan is about 1.5 s) and show a small spinner while scanning.
- Footer: "Checked (SHA-256), not yet encrypted." in secondary text (honest, and it becomes "Encrypted" when encryption lands), Cancel (secondary) and Send (primary) dialog buttons.
- Progress: a file chip, a 12px straw progress bar, and the numbers from \`drop: [####......]  40%  416 KiB / 1024 KiB\`. Cancel stops the transfer. \`drop: sent ...\` (exit 0) closes the sheet and shows the Received toast on the other machine; \`drop: <reason>\` (exit 1) shows the reason in an error row.
- Incoming (a milkOS-owned panel in the Powerbox style, 644 wide): "alice wants to send you bike.png" with the size, and for a new device a honey panel with the fingerprint at 4x text, "Check this matches what alice sees", and "Accepting trusts this device from now on." Buttons: Decline (secondary) and Trust alice and accept (primary). A known device shows no panel and the buttons Decline and Accept. Driven by \`drop recv\`, which prints "wants to send you" then waits at "accept? [y/N]"; the panel writes y or n.
- Key changed: a danger-coloured header ("bob's key has changed", "Sending is blocked until you decide"), the old and new fingerprints side by side, advice to compare them in person or by phone, and one danger button, Forget bob and re-verify (\`drop forget bob\`). Sending stays blocked until then.
- Received toast: the Notification drawer's toast with two pill buttons, Show (in Files) and Open. Files land in ~/Downloads.
- Nothing nearby: "No other milkOS machines found" with Look again and Close.

## Shield (design only, parked, not scheduled)
Mockups: shield.html. A read-only view of what each app can do and what it was refused. Nothing here needs new enforcement: it reads the capability sets and the audit log (docs/security.md). Any control that would change network access is drawn disabled and marked "parked, not scheduled". Do not queue engineering for this until the project owner says so. The first buildable version could be read-only: a title-bar button opening a panel (files, files you gave it, network, sound, clipboard, windows, recent activity), and Settings > Privacy with the audit log as sentences and an "intact" line from \`audit verify\`.

## What the mockup does not show
Hover/pressed states (lighten fills 4% / darken 4%), animation (none required for 1.0; Spaces can snap), cursor, lock screen, text selection colour (straw at 27%). Raise it with the design owner if one of these blocks you.
`;

const here = dirname(fileURLToPath(import.meta.url));
writeFileSync(join(here, "milkos-1.0.html"), page);
writeFileSync(join(here, "SPEC.md"), spec);
console.log("wrote milkos-1.0.html", page.length, "bytes; SPEC.md", spec.length, "bytes");

export { WP, glass, shadow, menubar, dock, win, lightsX, filesWin, shellWin, dim, toast, col };
