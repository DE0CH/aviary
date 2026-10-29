#!/usr/bin/env node
// Prints every web chunk of a build with its raw, gzip -9 and brotli q4 size, and checks the loading budgets
// (docs/paseo-ui.md "Loading"): the entry chunk (what the page waits for before the app's message box) and the
// reply chunk (the reply's display). With --strict a budget miss, or a chunk Metro split out for modules two lazy
// chunks share (__common), fails. Usage: node scripts/check-chunks.mjs [dist] [--strict]
import { readdirSync, readFileSync } from 'node:fs';
import path from 'node:path';
import { brotliCompressSync, constants, gzipSync } from 'node:zlib';

const args = process.argv.slice(2);
const strict = args.includes('--strict');
const dist = path.resolve(args.find((arg) => !arg.startsWith('--')) ?? 'dist');
const dir = path.join(dist, '_expo', 'static', 'js', 'web');
const BUDGET_KB = { entry: 1250, 'reply-chunk': 450 };

const html = readFileSync(path.join(dist, 'index.html'), 'utf8');
const entryFiles = new Set([...html.matchAll(/_expo\/static\/js\/web\/([^"']+\.js)/g)].map((m) => m[1]));
const kb = (bytes) => bytes / 1024;
const rows = readdirSync(dir)
  .filter((name) => name.endsWith('.js'))
  .map((name) => {
    const source = readFileSync(path.join(dir, name));
    return {
      name,
      role: entryFiles.has(name) ? 'entry' : name.replace(/-[0-9a-f]{32}\.js$/, ''),
      raw: source.length,
      gzip: gzipSync(source, { level: 9 }).length,
      br: brotliCompressSync(source, { params: { [constants.BROTLI_PARAM_QUALITY]: 4 } }).length,
    };
  })
  .sort((a, b) => b.br - a.br);

console.log('chunk'.padEnd(44), 'raw KB'.padStart(9), 'gzip-9 KB'.padStart(10), 'br-q4 KB'.padStart(9));
for (const row of rows) {
  console.log(
    `${row.role === 'entry' ? '* ' : '  '}${row.name}`.padEnd(44).slice(0, 44),
    kb(row.raw).toFixed(1).padStart(9),
    kb(row.gzip).toFixed(1).padStart(10),
    kb(row.br).toFixed(1).padStart(9),
  );
}
console.log('(* = loaded by index.html)');

const problems = [];
const entryBr = rows.filter((row) => row.role === 'entry').reduce((sum, row) => sum + row.br, 0);
if (kb(entryBr) > BUDGET_KB.entry) problems.push(`entry ${kb(entryBr).toFixed(0)} KB brotli > ${BUDGET_KB.entry} KB`);
for (const row of rows) {
  const budget = BUDGET_KB[row.role];
  if (row.role !== 'entry' && budget && kb(row.br) > budget) {
    problems.push(`${row.role} ${kb(row.br).toFixed(0)} KB brotli > ${budget} KB`);
  }
  if (/^__(common|expo-metro-runtime)/.test(row.name)) problems.push(`${row.name}: modules shared by lazy chunks`);
}
for (const problem of problems) console.log(`${strict ? 'FAIL' : 'over budget'}: ${problem}`);
if (strict && problems.length > 0) process.exit(1);
