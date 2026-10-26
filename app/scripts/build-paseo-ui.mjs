#!/usr/bin/env node
// Builds the staff UI, the Paseo web app under paseo/ (docs/paseo-ui.md), and puts it where the server serves the
// browser UI from: dist/. paseo/ has its own npm workspace and lockfile; install it first (`npm --prefix paseo ci`).
import { spawnSync } from 'node:child_process';
import { cpSync, rmSync } from 'node:fs';
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
console.log('Paseo UI copied to dist/');
