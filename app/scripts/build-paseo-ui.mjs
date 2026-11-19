#!/usr/bin/env node
// Builds the staff UI, the Paseo web app under paseo/ (docs/paseo-ui.md), and puts it where the server serves the
// browser UI from: dist/. paseo/ has its own npm workspace and lockfile; install it first (`npm --prefix paseo ci`).
import { spawnSync } from 'node:child_process';
import { cpSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const env = {
  ...process.env,
  // Metro's transform workers hold the 10 MB generated protocol validator; the default heap is too small for it
  NODE_OPTIONS: [process.env.NODE_OPTIONS, '--max-old-space-size=6144'].filter(Boolean).join(' '),
};
const built = spawnSync('npm', ['--prefix', path.join(root, 'paseo'), 'run', 'build:web'], { stdio: 'inherit', env });
if (built.status !== 0) process.exit(built.status ?? 1);
rmSync(path.join(root, 'dist'), { recursive: true, force: true });
cpSync(path.join(root, 'paseo', 'packages', 'app', 'dist'), path.join(root, 'dist'), { recursive: true });
// The terminal's stylesheet is the only one Expo hoists into the page, and a <link rel="stylesheet"> holds the first
// paint (the page frame in index.html) for a round trip. The terminal loads on demand, so its CSS loads without
// blocking: media="print" until it arrives (docs/paseo-ui.md "Loading").
const indexPath = path.join(root, 'dist', 'index.html');
const html = readFileSync(indexPath, 'utf8')
  .replace(/<link rel="preload" href="([^"]+\.css)" as="style">\n?/g, '')
  .replace(/<link rel="stylesheet" href="([^"]+\.css)">/g, '<link rel="stylesheet" href="$1" media="print" onload="this.media=\'all\'">');
writeFileSync(indexPath, html);
console.log('Paseo UI copied to dist/');
// Chunk sizes and the loading budgets (docs/paseo-ui.md "Loading")
spawnSync(process.execPath, [path.join(root, 'scripts', 'check-chunks.mjs'), path.join(root, 'dist')], {
  stdio: 'inherit',
});
