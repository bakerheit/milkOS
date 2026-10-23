// Runs the module conformance cases (user/jsengine/tests/modules/*.txt) under Node and compares
// what each case logs with what the case expects, so the expectations are checked against a
// second implementation of the spec. Cases that expect the test host's own lines (link error:,
// eval error:, ...) or are marked `//# oracle: no` are skipped: Node words its errors differently.
// Usage: node scripts/jsmodules_oracle.mjs [file.txt ...]
import { readFileSync, readdirSync, mkdtempSync, writeFileSync, mkdirSync, rmSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { tmpdir } from 'node:os';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const dir = join(here, '..', 'user', 'jsengine', 'tests', 'modules');
const args = process.argv.slice(2);
const files = args.length ? args : readdirSync(dir).filter((f) => f.endsWith('.txt'));

function parse(text) {
  const cases = [];
  let mode = 'none';
  for (const line of text.split('\n')) {
    if (line.startsWith('//# ')) {
      const rest = line.slice(4);
      if (rest.startsWith('case: ')) { cases.push({ name: rest.slice(6).trim(), files: [], expect: [], oracle: true }); mode = 'none'; continue; }
      if (rest.startsWith('file: ')) { cases.at(-1).files.push([rest.slice(6).trim(), '']); mode = 'file'; continue; }
      if (rest.trim() === 'expect') { mode = 'expect'; continue; }
      if (rest.trim() === 'oracle: no') { cases.at(-1).oracle = false; continue; }
    }
    const c = cases.at(-1);
    if (!c) continue;
    if (mode === 'file') c.files.at(-1)[1] += line + '\n';
    else if (mode === 'expect' && line !== '') c.expect.push(line);
  }
  return cases;
}

const HOST_LINES = /^(link error|eval error|load error|unhandled rejection|job error|evaluate failed|script error)/;
let ran = 0, skipped = 0, bad = 0;
for (const f of files) {
  const text = readFileSync(f.includes('/') ? f : join(dir, f), 'utf8');
  for (const c of parse(text)) {
    if (!c.oracle || c.expect.some((l) => HOST_LINES.test(l))) { skipped++; continue; }
    const tmp = mkdtempSync(join(tmpdir(), 'jsmod-'));
    try {
      writeFileSync(join(tmp, 'package.json'), '{"type":"module"}');
      writeFileSync(join(tmp, 'preload.mjs'),
        "import util from 'node:util';\n" +
        "globalThis.log = (...a) => console.log(a.map((x) => typeof x === 'string' ? x : (x === null || x === undefined || typeof x !== 'object' && typeof x !== 'function' ? String(x) : util.inspect(x))).join(' '));\n");
      for (const [name, src] of c.files) {
        const p = join(tmp, name);
        mkdirSync(dirname(p), { recursive: true });
        writeFileSync(p, src);
      }
      // A case whose entry is a classic script (script.js) runs as one; a module otherwise.
      const entry = c.files.some(([n]) => n === 'main.js') ? 'main.js' : 'script.cjs';
      if (entry === 'script.cjs') {
        const src = c.files.find(([n]) => n === 'script.js')[1];
        writeFileSync(join(tmp, 'script.cjs'), src);
      }
      const r = spawnSync(process.execPath, ['--import', './preload.mjs', entry], { cwd: tmp, encoding: 'utf8', timeout: 20000 });
      const got = r.stdout.split('\n').filter((l) => l !== '');
      ran++;
      if (JSON.stringify(got) !== JSON.stringify(c.expect)) {
        bad++;
        console.log(`MISMATCH ${f}: ${c.name}\n  expected: ${JSON.stringify(c.expect)}\n  node:     ${JSON.stringify(got)}${r.status ? '\n  stderr: ' + r.stderr.split('\n').slice(0, 6).join(' | ') : ''}`);
      }
    } finally {
      rmSync(tmp, { recursive: true, force: true });
    }
  }
}
console.log(`jsmodules-oracle: ${ran} compared with node, ${skipped} skipped, ${bad} differ`);
process.exit(bad ? 1 : 0);
