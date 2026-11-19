#!/usr/bin/env node
// Copies the hashed web assets out of yyc-ai-ui images (pigeon spec 17.1 step 6, cutover D13): for each tag,
// /app/dist/_expo/static/** and /app/dist/assets/** of ghcr.io/de0ch/yyc-ai-ui:<tag> (linux/amd64) into <dir>/<tag>/,
// with the image digest in <dir>/<tag>/.digest (written last). A pre-Paseo image has no _expo/ tree and gives an
// empty seed.
//
//   node fetch-seeds.mjs --dir DIR [--trusted SEEDS_JSON] TAG...      (crane on PATH, logged in to ghcr.io)
//
// <dir> may hold a cached copy from an earlier run (the workflow's actions/cache). The cache is not trusted: any job
// of the branch can write a cache entry, the build job included, which runs the app's npm dependencies. A cached
// <tag>/ is used only when its .digest is the digest the registry gives for the tag now (the tag is mutable: a re-run
// of jarvis-image re-pushes it with a different export) AND its files are exactly the files and sha256 that
// SEEDS_JSON (meta/seeds.json of the previous pigeon-app image, written by the image job from its own fresh exports)
// records for that digest. Anything else is fetched again from the registry. A missing image or a failed pull fails
// the script.
import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

import { walk } from './assemble.mjs';

export const UI_IMAGE = 'ghcr.io/de0ch/yyc-ai-ui';
const PARALLEL = 4;
const sha256 = (file) => createHash('sha256').update(readFileSync(file)).digest('hex');

function run(command, args) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { stdio: ['ignore', 'pipe', 'pipe'] });
    let out = '';
    let err = '';
    child.stdout.on('data', (chunk) => (out += chunk));
    child.stderr.on('data', (chunk) => (err += chunk));
    child.on('error', reject);
    child.on('close', (code) => (code === 0 ? resolve(out) : reject(new Error(`${command} ${args.join(' ')}: exit ${code}\n${err}`))));
  });
}

// `crane export … - | tar -x -C tree app/dist`: the flattened filesystem streamed, only /app/dist written
function exportDist(ref, tree) {
  return new Promise((resolve, reject) => {
    const crane = spawn('crane', ['export', '--platform', 'linux/amd64', ref, '-'], { stdio: ['ignore', 'pipe', 'pipe'] });
    const tar = spawn('tar', ['-x', '-C', tree, 'app/dist'], { stdio: ['pipe', 'ignore', 'pipe'] });
    let err = '';
    crane.stderr.on('data', (chunk) => (err += chunk));
    tar.stderr.on('data', (chunk) => (err += chunk));
    crane.stdout.pipe(tar.stdin);
    const codes = {};
    const done = (name) => (code) => {
      codes[name] = code;
      if (!('crane' in codes) || !('tar' in codes)) return;
      if (codes.crane === 0 && codes.tar === 0) resolve();
      else reject(new Error(`crane export ${ref} | tar: exit ${codes.crane}/${codes.tar}\n${err}`));
    };
    crane.on('error', reject);
    tar.on('error', reject);
    crane.on('close', done('crane'));
    tar.on('close', done('tar'));
  });
}

// the cached copy is the image's current digest and holds exactly the files the trusted manifest records for it
export function cachedCopyValid(dir, digest, trusted) {
  if (!existsSync(path.join(dir, '.digest'))) return { ok: false, why: 'not cached' };
  const cached = readFileSync(path.join(dir, '.digest'), 'utf8').trim();
  if (cached !== digest) return { ok: false, why: `cached digest ${cached} is not the tag's ${digest}` };
  const record = [trusted, ...(trusted?.older ?? [])].find((entry) => entry?.digest === digest);
  if (!record) return { ok: false, why: 'no trusted manifest for this digest' };
  const files = walk(dir).filter((rel) => rel !== '.digest');
  const expected = Object.keys(record.files).sort();
  if (files.length !== expected.length || files.some((rel, i) => rel !== expected[i])) return { ok: false, why: 'the cached file list differs from the manifest' };
  const bad = files.find((rel) => sha256(path.join(dir, rel)) !== record.files[rel]);
  if (bad) return { ok: false, why: `${bad} differs from the manifest` };
  return { ok: true };
}

// moves a fresh export's hashed trees into <out> (a pre-Paseo image's dist/assets is the CloudCLI UI's, which cannot
// talk to pigeon, so it is not kept)
export function takeHashedTrees(exported, out) {
  rmSync(out, { recursive: true, force: true });
  mkdirSync(out, { recursive: true });
  const dist = path.join(exported, 'app', 'dist');
  if (existsSync(path.join(dist, '_expo', 'static'))) {
    mkdirSync(path.join(out, '_expo'));
    renameSync(path.join(dist, '_expo', 'static'), path.join(out, '_expo', 'static'));
    if (existsSync(path.join(dist, 'assets'))) renameSync(path.join(dist, 'assets'), path.join(out, 'assets'));
  }
}

async function fetchOne(dir, tag, trusted) {
  const out = path.join(dir, tag);
  const digest = (await run('crane', ['digest', '--platform', 'linux/amd64', `${UI_IMAGE}:${tag}`])).trim();
  if (!/^sha256:[0-9a-f]{64}$/.test(digest)) throw new Error(`${UI_IMAGE}:${tag}: crane digest gave ${digest}`);
  const check = cachedCopyValid(out, digest, trusted[tag]);
  if (check.ok) {
    console.log(`cached  ${tag} ${digest} (${walk(out).length - 1} files, as the previous image's seeds.json records)`);
    return;
  }
  const scratch = mkdtempSync(path.join(dir, `.fetch-${tag}.`));
  try {
    // every ui image has /app/dist; the hashed trees are picked from it
    await exportDist(`${UI_IMAGE}@${digest}`, scratch);
    takeHashedTrees(scratch, out);
    writeFileSync(path.join(out, '.digest'), `${digest}\n`);
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
  console.log(`fetched ${tag} ${digest} (${walk(out).length - 1} files; ${check.why})`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const args = process.argv.slice(2);
  let dir = null;
  let trustedFile = null;
  const tags = [];
  for (let i = 0; i < args.length; i++) {
    if (args[i] === '--dir') dir = args[++i];
    else if (args[i] === '--trusted') trustedFile = args[++i];
    else if (/^[0-9a-f]{12,40}$/.test(args[i])) tags.push(args[i]);
    else throw new Error(`fetch-seeds: unknown argument ${args[i]}`);
  }
  if (!dir) {
    console.error('usage: node fetch-seeds.mjs --dir DIR [--trusted SEEDS_JSON] TAG...');
    process.exit(2);
  }
  mkdirSync(dir, { recursive: true });
  const trusted = trustedFile && existsSync(trustedFile) ? JSON.parse(readFileSync(trustedFile, 'utf8')) : {};
  const queue = [...new Set(tags)];
  let failed = false;
  await Promise.all(
    Array.from({ length: PARALLEL }, async () => {
      while (queue.length) {
        const tag = queue.shift();
        try {
          await fetchOne(dir, tag, trusted);
        } catch (error) {
          failed = true;
          console.error(`::error::seed ${tag}: ${error.message}`);
        }
      }
    }),
  );
  process.exit(failed ? 1 : 0);
}
