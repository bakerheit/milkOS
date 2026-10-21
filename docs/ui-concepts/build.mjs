// Builds index.html from scenes.mjs. Run: node docs/ui-concepts/build.mjs
// The output needs no JavaScript to flip between concepts (radio buttons + CSS), and each mockup is an
// inline SVG so it scales to any width. A tiny script only adds arrow-key flipping when scripts are allowed.
import { CONCEPTS } from "./scenes.mjs";
import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const N = CONCEPTS.length;
const id = (i) => `c${String(((i % N) + N) % N + 1).padStart(2, "0")}`;
const esc = (s) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;");

const scene = (c, cls = "") =>
  `<svg class="scene ${cls}" viewBox="0 0 1024 768" preserveAspectRatio="xMidYMid meet" role="img" aria-label="Mockup of concept ${c.n}, ${c.name}"><foreignObject x="0" y="0" width="1024" height="768"><div class="stage">${c.draw()}</div></foreignObject></svg>`;

const radios = CONCEPTS.map((c, i) => `<input class="r" type="radio" name="concept" id="r-${id(i)}"${i === 0 ? " checked" : ""}>`).join("\n");

const panels = CONCEPTS.map((c, i) => `
<article class="panel p-${id(i)}">
  <div class="stagebox">
    <div class="stagewrap">${scene(c)}</div>
    <div class="stagecap"><span>Drawn 1:1 at 1024×768 and scaled to fit. Text sits on the 8×16 Unifont grid (a fallback mono font stands in here).</span><span>${c.n} ${c.name}</span></div>
  </div>
  <aside class="notes">
    <h2>${c.n} ${c.name}</h2><p class="tag">${c.tag}</p>
    <span class="cost ${c.cost}">${c.cost}</span>
    <h3>Key idea</h3><p>${c.idea}</p>
    <h3>Cost to build in a kernel-drawn UI</h3><p>${c.why}</p>
    <h3>Main elements</h3><ul class="chips">${c.els.map((e) => `<li>${e}</li>`).join("")}</ul>
    <h3>Font note</h3><p>${c.font}</p>
  </aside>
</article>`).join("\n");

const navs = CONCEPTS.map((c, i) => `
<div class="nav n-${id(i)}">
  <label class="btn" for="r-${id(i - 1)}" role="button" tabindex="0" aria-label="Previous concept">&larr; Prev</label>
  <span class="count" aria-live="polite">${i + 1} / ${N}</span>
  <label class="btn" for="r-${id(i + 1)}" role="button" tabindex="0" aria-label="Next concept">Next &rarr;</label>
</div>`).join("\n");

const thumbs = CONCEPTS.map((c, i) => `
<label class="thumb t-${id(i)}" for="r-${id(i)}" tabindex="0" role="button" aria-label="Show concept ${c.n}, ${c.name}, ${c.cost} to build">
  <div class="tw">${scene(c, "ti")}</div>
  <div class="tl"><b>${c.n} ${c.name}</b><span><i class="dot ${c.cost}"></i>${c.cost}</span></div>
</label>`).join("\n");

const cmp = CONCEPTS.map((c) => `<tr><td>${c.n}</td><td><b>${c.name}</b><br><span class="small">${c.tag}</span></td><td><span class="cost ${c.cost}" style="font-size:13px">${c.cost}</span></td><td>${c.why}</td></tr>`).join("\n");

// show-rules per concept: only the checked one's panel + nav are visible, its thumb gets the highlight
const rules = CONCEPTS.map((c, i) => `#r-${id(i)}:checked ~ .wrap .p-${id(i)}, #r-${id(i)}:checked ~ .wrap .n-${id(i)}{display:grid}
#r-${id(i)}:checked ~ .wrap .n-${id(i)}{display:flex}
#r-${id(i)}:checked ~ .wrap .t-${id(i)}{border-color:var(--accent)}`).join("\n");

const html = `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>milkOS UI Concepts</title>
<style>
:root{--bg:#FBF6EC;--card:#FFFDF8;--line:#E2D5BD;--ink:#3A2E28;--mute:#7A6652;--accent:#C9684F;--chip:#F1E8D6;--easy:#4F8A5B;--med:#B8832A;--hard:#B5473B}
@media (prefers-color-scheme:dark){:root{--bg:#1E1915;--card:#2A231D;--line:#443A30;--ink:#F3EBDD;--mute:#B9A68C;--accent:#E9966F;--chip:#383028;--easy:#7FC08B;--med:#E2B359;--hard:#E8826F}}
*{box-sizing:border-box}
html,body{margin:0}
body{background:var(--bg);color:var(--ink);font:16px/1.5 ui-sans-serif,system-ui,-apple-system,"Segoe UI",sans-serif}
.r{position:absolute;opacity:0;pointer-events:none}
.wrap{max-width:1400px;margin:0 auto;padding:20px 16px 48px}
header{display:flex;flex-wrap:wrap;gap:12px;align-items:baseline;justify-content:space-between;margin-bottom:16px}
h1{font-size:22px;margin:0;font-weight:700;letter-spacing:-.01em}
.sub{color:var(--mute);font-size:14px;max-width:760px}
.nav{display:none;gap:8px;align-items:center}
.btn{display:inline-block;font:inherit;color:var(--ink);background:var(--card);border:1px solid var(--line);border-radius:10px;padding:8px 16px;cursor:pointer;min-height:44px;line-height:26px;user-select:none}
.btn:hover{border-color:var(--accent)}
.btn:focus-visible,.thumb:focus-visible{outline:3px solid var(--accent);outline-offset:2px}
.count{min-width:64px;text-align:center;color:var(--mute);font-variant-numeric:tabular-nums}
.panel{display:none;grid-template-columns:minmax(0,1fr) 360px;gap:20px;align-items:start}
@media(max-width:980px){.panel{grid-template-columns:1fr}}
.stagebox{background:var(--card);border:1px solid var(--line);border-radius:14px;padding:12px}
.stagewrap{border-radius:6px;border:1px solid var(--line);overflow:hidden;line-height:0}
.scene{display:block;width:100%;height:auto}
.stage{width:1024px;height:768px;position:relative;overflow:hidden;font:13.333px/16px "Unifont","Menlo","SF Mono",ui-monospace,monospace;color:#3A2E28}
.stage div{box-sizing:border-box}
.stagecap{display:flex;justify-content:space-between;gap:12px;color:var(--mute);font-size:13px;margin-top:8px}
.notes{background:var(--card);border:1px solid var(--line);border-radius:14px;padding:18px}
.notes h2{margin:0 0 2px;font-size:20px}
.notes .tag{color:var(--mute);margin:0 0 12px;font-size:14px}
.notes h3{font-size:12px;text-transform:uppercase;letter-spacing:.08em;color:var(--mute);margin:16px 0 6px}
.notes p{margin:0}
.cost{display:inline-block;font-weight:700;border-radius:999px;padding:2px 12px;border:1.5px solid currentColor;font-size:14px}
.cost.easy{color:var(--easy)}.cost.medium{color:var(--med)}.cost.hard{color:var(--hard)}
.chips{display:flex;flex-wrap:wrap;gap:6px;padding:0;margin:0;list-style:none}
.chips li{background:var(--chip);border-radius:8px;padding:2px 9px;font-size:14px}
.strip{display:grid;grid-template-columns:repeat(5,minmax(0,1fr));gap:10px;margin-top:20px}
@media(max-width:700px){.strip{grid-template-columns:repeat(2,minmax(0,1fr))}}
.thumb{display:block;cursor:pointer;border:2px solid var(--line);border-radius:10px;background:var(--card);padding:6px}
.thumb .tw{border-radius:4px;overflow:hidden;line-height:0}
.thumb .tl{display:flex;justify-content:space-between;gap:6px;font-size:13px;margin-top:5px;padding:0 2px}
.dot{display:inline-block;width:9px;height:9px;border-radius:50%;margin-right:5px}
.dot.easy{background:var(--easy)}.dot.medium{background:var(--med)}.dot.hard{background:var(--hard)}
section.plan{margin-top:28px;background:var(--card);border:1px solid var(--line);border-radius:14px;padding:18px 20px}
section.plan h2{margin:0 0 8px;font-size:18px}
section.plan ol{margin:8px 0 0;padding-left:22px}
section.plan li{margin:4px 0}
table{border-collapse:collapse;width:100%;margin-top:10px;font-size:14px}
th,td{text-align:left;padding:6px 8px;border-bottom:1px solid var(--line);vertical-align:top}
th{color:var(--mute);font-weight:600}
.small{font-size:13px;color:var(--mute)}
${rules}
</style>
</head>
<body>
${radios}
<div class="wrap">
  <header>
    <div>
      <h1>milkOS: 10 concept UIs</h1>
      <div class="sub">1024×768, 32-bit, drawn by our own kernel. Everything below uses only rectangles, rounded rects, text, simple icons and alpha. Use Prev / Next or tap a thumbnail.</div>
    </div>
    ${navs}
  </header>

  ${panels}

  <div class="strip" role="list">${thumbs}</div>

  <section class="plan">
    <h2>Mary's take: what to build first</h2>
    <p>Picking one look is not the same as picking a build order. Every concept needs the same four things underneath, so those come first no matter which look wins:</p>
    <ol>
      <li><b>Framebuffer + rect/rounded-rect/text/alpha primitives</b> with a dirty-rectangle redraw. Concepts 2, 5 and 8 are almost only this.</li>
      <li><b>Window manager basics:</b> a window list, z-order, focus, drag by title bar, close. Needed by 1, 2, 7, 8, 9, 10.</li>
      <li><b>Input:</b> keyboard first (every concept works with it), then a pointer, then touch (only concept 4 needs touch to feel right).</li>
      <li><b>Launcher + status bar</b> as the first real shell. Concept 3's launcher drops into any of the others.</li>
    </ol>
    <p style="margin-top:12px"><b>My pick:</b> ship <b>08 Platinum</b> or <b>02 Pail</b> as the first shell because they are cheap and honest about Unifont, then grow toward <b>01 Whole Milk</b> with <b>03 Skim</b> as its launcher. Keep <b>10 Glass</b> and <b>07 Spaces</b> as later polish: they need offscreen window buffers and scaled blits, which is where the real cost is.</p>
    <table>
      <thead><tr><th>#</th><th>Concept</th><th>Cost</th><th>The expensive part</th></tr></thead>
      <tbody>${cmp}</tbody>
    </table>
    <p class="small" style="margin-top:12px">Cost key: <b>easy</b> = a weekend of drawing code on top of the primitives. <b>medium</b> = needs a real window/compositor model or an extra input path. <b>hard</b> = needs offscreen buffers, scaling or animation timing the kernel does not have yet.</p>
  </section>
</div>
<script>
// Optional: arrow keys and Enter/Space on the label buttons. The page works without this.
(function(){
  var r=[].slice.call(document.querySelectorAll('.r'));
  function cur(){for(var i=0;i<r.length;i++)if(r[i].checked)return i;return 0}
  function go(i){r[(i+r.length)%r.length].checked=true}
  addEventListener('keydown',function(e){
    if(e.key==='ArrowRight')go(cur()+1);
    else if(e.key==='ArrowLeft')go(cur()-1);
    else if((e.key==='Enter'||e.key===' ')&&e.target.tagName==='LABEL'){e.preventDefault();e.target.click();}
  });
})();
</script>
</body>
</html>
`;

writeFileSync(join(here, "index.html"), html);
console.log("wrote index.html", html.length, "bytes");
