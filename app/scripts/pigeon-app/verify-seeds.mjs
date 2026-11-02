#!/usr/bin/env node
// Checks that a pigeon-app tree holds every hashed asset of each seeded yyc-ai-ui image byte for byte (WP-27
// acceptance, cutover D13 and its checklist): every file of <seeds>/<tag>/ (fetch-seeds.sh output: the image's
// /app/dist/_expo/static/** and /app/dist/assets/**) must exist at the same path under <app> with the same sha256.
//
//   node verify-seeds.mjs <app dir> <seeds dir> <tag>...
//
// Exit 0: all present and identical. 1: a file missing or different. 2: usage or a seed not fetched.
import { createHash } from 'node:crypto';
import { existsSync, readFileSync } from 'node:fs';
import path from 'node:path';

import { walk } from './assemble.mjs';

const [app, seeds, ...tags] = process.argv.slice(2);
if (!app || !seeds) {
  console.error('usage: node verify-seeds.mjs <app dir> <seeds dir> <tag>...');
  process.exit(2);
}
const sha256 = (file) => createHash('sha256').update(readFileSync(file)).digest('hex');
let bad = 0;
for (const tag of tags) {
  const dir = path.join(seeds, tag);
  if (!existsSync(path.join(dir, '.digest'))) {
    console.error(`verify-seeds: ${tag} was not fetched`);
    process.exit(2);
  }
  const files = walk(dir).filter((rel) => rel !== '.digest');
  const problems = [];
  for (const rel of files) {
    const target = path.join(app, rel);
    if (!existsSync(target)) problems.push(`missing ${rel}`);
    else if (sha256(target) !== sha256(path.join(dir, rel))) problems.push(`different ${rel}`);
  }
  const statics = files.filter((rel) => rel.startsWith('_expo/static/')).length;
  console.log(`${problems.length ? 'FAIL' : 'ok  '} yyc-ai-ui:${tag}: ${files.length} files (${statics} under _expo/static), ${problems.length} missing or different`);
  for (const problem of problems.slice(0, 20)) console.log(`     ${problem}`);
  bad += problems.length;
}
process.exit(bad ? 1 : 0);
