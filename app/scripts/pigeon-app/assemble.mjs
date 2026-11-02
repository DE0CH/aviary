#!/usr/bin/env node
// Assembles the files of a pigeon-app image (pigeon spec 17.1 steps 6-7, cutover D13, DESIGN 9.6): the new web export,
// plus the hashed assets of recent builds and of every seeded yyc-ai-ui image, so a tab loaded from any of them keeps
// fetching its own lazy chunks from pigeon.
//
//   node assemble.mjs --dist DIR --sw FILE --commit SHA --out DIR [--previous DIR] [--seeds DIR --seed TAG=COMMIT]...
//                     [--now ISO]
//
// --dist      the export (scripts/build-paseo-ui.mjs writes dist/)
// --sw        public/sw.js, served at /sw.js (the old service worker's kill switch)
// --previous  the filesystem of the previous pigeon-app image (its app/ and meta/), absent on the first build
// --seeds     a directory of fetch-seeds.sh output, one <tag>/ per yyc-ai-ui image; --seed names each image served
//             within the Access session lifetime with its claudecodeui commit
// --out       writes <out>/app (served, copied by the image's /cp into pigeon's /srv/app) and <out>/meta (not served):
//             assets-history.json {file: firstSeenISO}, builds.json (the builds whose assets are kept, newest first,
//             each with its own hashed files and the seeds it listed) and seeds.json (each seed's files and sha256)
//
// Hashed assets are the files under _expo/static/ and assets/ (their names carry a content hash). Kept: every file of
// the new build, of every seed listed now, and of every build among the last 5 or built within the last 30 days (with
// the seeds those builds listed). A kept path whose bytes differ between two sources (a chunk name reused for other
// content) fails the assembly: no tab could be served both. Finally every js/css/html/json/svg/wasm file gets a .br
// (quality 11) and a .gz (level 9) sibling, reused from the previous image when the file's bytes are unchanged.
import { createHash } from 'node:crypto';
import { cpSync, existsSync, mkdirSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { brotliCompressSync, constants, gzipSync } from 'node:zlib';

export const HASHED_TREES = ['_expo/static/', 'assets/'];
export const KEEP_BUILDS = 5;
export const KEEP_DAYS = 30;
const COMPRESSIBLE = /\.(js|css|html|json|svg|wasm)$/;
const SIBLING = /\.(br|gz)$/;

const isHashed = (rel) => HASHED_TREES.some((tree) => rel.startsWith(tree));
const sha256 = (file) => createHash('sha256').update(readFileSync(file)).digest('hex');

// relative paths of the regular files under dir, '/'-separated, sorted
export function walk(dir) {
  const out = [];
  const visit = (sub) => {
    for (const entry of readdirSync(path.join(dir, sub), { withFileTypes: true })) {
      const rel = sub ? `${sub}/${entry.name}` : entry.name;
      if (entry.isDirectory()) visit(rel);
      else if (entry.isFile()) out.push(rel);
    }
  };
  if (existsSync(dir)) visit('');
  return out.sort();
}

const readJson = (file, fallback) => (existsSync(file) ? JSON.parse(readFileSync(file, 'utf8')) : fallback);

export function assemble({ dist, sw, previous = null, seedsDir = null, seeds = [], commit, now = new Date(), out }) {
  if (!/^[0-9a-f]{40}$/.test(commit)) throw new Error(`--commit ${commit}: not a 40-hex commit`);
  const nowIso = now.toISOString();
  const app = path.join(out, 'app');
  const meta = path.join(out, 'meta');
  rmSync(out, { recursive: true, force: true });
  mkdirSync(meta, { recursive: true });
  cpSync(dist, app, { recursive: true });
  cpSync(sw, path.join(app, 'sw.js'));

  const own = walk(app).filter(isHashed);
  if (!own.some((rel) => rel.startsWith('_expo/static/js/'))) throw new Error(`${dist}: no _expo/static/js (not a web export)`);

  const prevApp = previous ? path.join(previous, 'app') : null;
  const prevMeta = previous ? path.join(previous, 'meta') : null;
  if (previous && !existsSync(path.join(prevMeta, 'builds.json'))) throw new Error(`${previous}: no meta/builds.json`);
  const prevBuilds = previous ? readJson(path.join(prevMeta, 'builds.json'), []) : [];
  const prevSeeds = previous ? readJson(path.join(prevMeta, 'seeds.json'), {}) : {};
  const prevHistory = previous ? readJson(path.join(prevMeta, 'assets-history.json'), {}) : {};

  // the seeds listed now, from the fetched images
  const seedsNow = {};
  for (const { tag, commit: seedCommit } of seeds) {
    const dir = path.join(seedsDir, tag);
    if (!existsSync(path.join(dir, '.digest'))) throw new Error(`seed ${tag}: not fetched (${dir}/.digest missing)`);
    const files = {};
    for (const rel of walk(dir).filter(isHashed)) files[rel] = sha256(path.join(dir, rel));
    seedsNow[tag] = { commit: seedCommit, digest: readFileSync(path.join(dir, '.digest'), 'utf8').trim(), files };
  }

  // the builds whose assets stay: the newest KEEP_BUILDS and any built within KEEP_DAYS
  const cutoff = now.getTime() - KEEP_DAYS * 86400 * 1000;
  const builds = [
    { commit, builtAt: nowIso, files: own, seeds: seeds.map((seed) => seed.tag) },
    ...prevBuilds.filter((build) => build.commit !== commit),
  ].filter((build, index) => index < KEEP_BUILDS || new Date(build.builtAt).getTime() >= cutoff);

  const seedsMeta = {};
  for (const build of builds) {
    for (const tag of build.seeds) {
      const seed = seedsNow[tag] ?? prevSeeds[tag];
      if (!seed) throw new Error(`build ${build.commit.slice(0, 12)} lists seed ${tag}, which is neither fetched nor in the previous image's seeds.json`);
      seedsMeta[tag] = seed;
    }
  }

  // every kept path with the sources that hold it
  const ownSet = new Set(own);
  const kept = new Map(); // rel -> [{label, file, hash?}]
  const add = (rel, source) => {
    if (!kept.has(rel)) kept.set(rel, []);
    kept.get(rel).push(source);
  };
  for (const rel of own) add(rel, { label: `this build ${commit.slice(0, 12)}`, file: path.join(app, rel) });
  for (const [tag, seed] of Object.entries(seedsMeta)) {
    for (const [rel, hash] of Object.entries(seed.files)) {
      add(rel, { label: `yyc-ai-ui:${tag}`, file: seedsNow[tag] ? path.join(seedsDir, tag, rel) : null, hash });
    }
  }
  for (const build of builds.slice(1)) {
    for (const rel of build.files) add(rel, { label: `build ${build.commit.slice(0, 12)}`, file: null });
  }

  const conflicts = [];
  let copied = 0;
  for (const [rel, sources] of kept) {
    const prevFile = prevApp && existsSync(path.join(prevApp, rel)) ? path.join(prevApp, rel) : null;
    const hashes = new Map();
    for (const source of sources) {
      const file = source.file ?? prevFile;
      if (!file) throw new Error(`${rel} (${source.label}) is kept but the previous image does not hold it`);
      const hash = sha256(file);
      if (source.hash && source.hash !== hash) conflicts.push(`${rel}: ${source.label} recorded ${source.hash}, the file is ${hash}`);
      if (!hashes.has(hash)) hashes.set(hash, []);
      hashes.get(hash).push(source.label);
    }
    if (hashes.size > 1) {
      conflicts.push(`${rel}: ${[...hashes].map(([hash, labels]) => `${hash.slice(0, 12)} in ${labels.join(', ')}`).join('; ')}`);
      continue;
    }
    if (ownSet.has(rel)) continue;
    const from = sources.find((source) => source.file)?.file ?? prevFile;
    mkdirSync(path.dirname(path.join(app, rel)), { recursive: true });
    cpSync(from, path.join(app, rel));
    copied++;
  }
  if (conflicts.length) throw new Error(`hashed asset names with different bytes:\n  ${conflicts.join('\n  ')}`);

  const history = {};
  for (const rel of [...kept.keys()].sort()) history[rel] = prevHistory[rel] ?? nowIso;

  // .br and .gz siblings, reused when the previous image has them for the same bytes
  let compressed = 0;
  let reused = 0;
  for (const rel of walk(app)) {
    if (!COMPRESSIBLE.test(rel) || SIBLING.test(rel)) continue;
    const file = path.join(app, rel);
    const prev = prevApp ? path.join(prevApp, rel) : null;
    if (
      prev &&
      existsSync(prev) &&
      existsSync(`${prev}.br`) &&
      existsSync(`${prev}.gz`) &&
      sha256(prev) === sha256(file)
    ) {
      cpSync(`${prev}.br`, `${file}.br`);
      cpSync(`${prev}.gz`, `${file}.gz`);
      reused++;
      continue;
    }
    const source = readFileSync(file);
    writeFileSync(
      `${file}.br`,
      brotliCompressSync(source, {
        params: {
          [constants.BROTLI_PARAM_QUALITY]: 11,
          [constants.BROTLI_PARAM_SIZE_HINT]: source.length,
        },
      }),
    );
    writeFileSync(`${file}.gz`, gzipSync(source, { level: 9 }));
    compressed++;
  }

  writeFileSync(path.join(meta, 'assets-history.json'), `${JSON.stringify(history, null, 1)}\n`);
  writeFileSync(path.join(meta, 'builds.json'), `${JSON.stringify(builds, null, 1)}\n`);
  writeFileSync(path.join(meta, 'seeds.json'), `${JSON.stringify(seedsMeta, null, 1)}\n`);
  const bytes = walk(app).reduce((sum, rel) => sum + statSync(path.join(app, rel)).size, 0);
  return { own: own.length, kept: kept.size, copied, builds: builds.length, seeds: Object.keys(seedsMeta).length, compressed, reused, bytes };
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  const args = process.argv.slice(2);
  const opts = { seeds: [] };
  for (let i = 0; i < args.length; i++) {
    const value = args[++i];
    if (value === undefined) throw new Error(`${args[i - 1]} needs a value`);
    switch (args[i - 1]) {
      case '--dist': opts.dist = value; break;
      case '--sw': opts.sw = value; break;
      case '--previous': opts.previous = value; break;
      case '--seeds': opts.seedsDir = value; break;
      case '--seed': {
        const [tag, seedCommit] = value.split('=');
        if (!/^[0-9a-f]{12,40}$/.test(tag ?? '') || !/^[0-9a-f]{40}$/.test(seedCommit ?? '')) throw new Error(`--seed ${value}: not TAG=COMMIT`);
        opts.seeds.push({ tag, commit: seedCommit });
        break;
      }
      case '--commit': opts.commit = value; break;
      case '--now': opts.now = new Date(value); break;
      case '--out': opts.out = value; break;
      default: throw new Error(`unknown option ${args[i - 1]}`);
    }
  }
  for (const required of ['dist', 'sw', 'commit', 'out']) if (!opts[required]) throw new Error(`--${required} is required`);
  if (opts.seeds.length && !opts.seedsDir) throw new Error('--seed needs --seeds');
  const result = assemble(opts);
  console.log(
    `pigeon-app: ${result.own} hashed files of this build, ${result.kept} kept in all (${result.copied} from seeds and earlier builds), ` +
      `${result.builds} builds and ${result.seeds} seeds kept; ${result.compressed} compressed, ${result.reused} reused; ` +
      `${(result.bytes / 1048576).toFixed(1)} MiB`,
  );
}
