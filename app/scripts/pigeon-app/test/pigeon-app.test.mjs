// Tests of the pigeon-app bundle scripts (.github/workflows/pigeon-app.yml): node --test scripts/pigeon-app/test/
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { brotliDecompressSync, gunzipSync } from 'node:zlib';

import { assemble, walk } from '../assemble.mjs';
import { pinsInKustomization, servedImages } from '../ui-pins.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
const tmp = () => mkdtempSync(path.join(os.tmpdir(), 'pigeon-app-test-'));
const write = (file, text) => {
  mkdirSync(path.dirname(file), { recursive: true });
  writeFileSync(file, text);
};
const sha = (n) => n.toString(16).padStart(40, 'a');
const DAY = 86400 * 1000;

// ---------- ui-pins

const kustomization = (tag) => `resources: [ui.yaml]
images:
  - name: jarvis3-chat
    newName: ghcr.io/de0ch/jarvis3-chat
    newTag: 52c9b8c498b6
  # the UI image
  - name: yyc-ai-ui
    newName: ghcr.io/de0ch/yyc-ai-ui
    newTag: ${tag}
configMapGenerator:
  - name: jarvis-config
`;

function deployRepo(steps) {
  const repo = tmp();
  const git = (args, date) =>
    execFileSync('git', ['-C', repo, ...args], {
      env: { ...process.env, GIT_AUTHOR_DATE: date, GIT_COMMITTER_DATE: date, GIT_AUTHOR_NAME: 't', GIT_AUTHOR_EMAIL: 't@example.com', GIT_COMMITTER_NAME: 't', GIT_COMMITTER_EMAIL: 't@example.com' },
    });
  execFileSync('git', ['init', '-q', '-b', 'main', repo]);
  for (const step of steps) {
    for (const [file, text] of Object.entries(step.files)) {
      if (text === null) rmSync(path.join(repo, file));
      else write(path.join(repo, file), text);
    }
    git(['add', '-A'], step.date);
    git(['commit', '-q', '--allow-empty', '-m', 'step'], step.date);
  }
  return repo;
}

test('pinsInKustomization reads only the yyc-ai-ui entry', () => {
  assert.deepEqual(pinsInKustomization(kustomization('488a7736ef2c')), ['488a7736ef2c']);
  assert.deepEqual(pinsInKustomization('images:\n  - name: yyc-ai-ui\n    newName: x\n  - name: other\n    newTag: abc\n'), []);
  assert.deepEqual(pinsInKustomization('images:\n- name: "yyc-ai-ui"\n  newTag: "0123456789ab"  # pin\n'), ['0123456789ab']);
});

test('servedImages: every image whose pin overlaps the window, including the one in force at its start', () => {
  const now = new Date('2026-11-30T00:00:00Z');
  const repo = deployRepo([
    { date: '2026-10-01T00:00:00Z', files: { 'yyc-ai/deploy/kustomization.yaml': kustomization('aaaaaaaaaaa1') } },
    { date: '2026-10-20T00:00:00Z', files: { 'yyc-ai/deploy/kustomization.yaml': kustomization('aaaaaaaaaaa2') } },
    // in force at the window start (2026-10-31)
    { date: '2026-10-25T00:00:00Z', files: { 'yyc-ai/deploy/kustomization.yaml': kustomization('aaaaaaaaaaa3') } },
    { date: '2026-11-05T00:00:00Z', files: { 'yyc-ai/deploy/ui.yaml': 'kind: Deployment\n' } },
    { date: '2026-11-06T00:00:00Z', files: { 'news-stream/x.txt': 'unrelated\n' } },
    // the P1 layout move: the old stack under legacy/
    {
      date: '2026-11-10T00:00:00Z',
      files: {
        'yyc-ai/deploy/kustomization.yaml': 'resources: [legacy]\n',
        'yyc-ai/deploy/legacy/kustomization.yaml': kustomization('aaaaaaaaaaa4'),
      },
    },
    // rollback to an earlier image, then the pin removed (A6)
    { date: '2026-11-12T00:00:00Z', files: { 'yyc-ai/deploy/legacy/kustomization.yaml': kustomization('aaaaaaaaaaa3') } },
    { date: '2026-11-20T00:00:00Z', files: { 'yyc-ai/deploy/legacy/kustomization.yaml': null } },
  ]);
  const images = servedImages({ repo, now, hours: 720 });
  assert.deepEqual(
    images.map((image) => image.tag),
    ['aaaaaaaaaaa3', 'aaaaaaaaaaa4'],
  );
  const three = images.find((image) => image.tag === 'aaaaaaaaaaa3');
  assert.equal(new Date(three.from).toISOString(), '2026-10-25T00:00:00.000Z');
  assert.equal(new Date(three.until).toISOString(), '2026-11-20T00:00:00.000Z');
  // a longer window reaches the older pins; a shorter one only what is pinned now
  assert.deepEqual(servedImages({ repo, now, hours: 24 * 60 }).map((image) => image.tag), ['aaaaaaaaaaa1', 'aaaaaaaaaaa2', 'aaaaaaaaaaa3', 'aaaaaaaaaaa4']);
  assert.deepEqual(servedImages({ repo, now, hours: 24 }), []);
  // as the repository stood before the removal
  const live = servedImages({ repo, ref: 'HEAD~1', now: new Date('2026-11-15T00:00:00Z'), hours: 24 });
  assert.deepEqual(live.map((image) => [image.tag, image.until]), [['aaaaaaaaaaa3', null]]);
  rmSync(repo, { recursive: true, force: true });
});

test('servedImages refuses a tag that is not a commit hash', () => {
  const repo = deployRepo([{ date: '2026-11-01T00:00:00Z', files: { 'yyc-ai/deploy/kustomization.yaml': kustomization('latest') } }]);
  assert.throws(() => servedImages({ repo, now: new Date('2026-11-02T00:00:00Z') }), /not a commit hash/);
  rmSync(repo, { recursive: true, force: true });
});

// ---------- assemble

function exportDir(root, name, chunks) {
  const dist = path.join(root, name);
  write(path.join(dist, 'index.html'), `<html>${name}</html>`);
  write(path.join(dist, 'manifest.json'), '{}');
  for (const [rel, text] of Object.entries(chunks)) write(path.join(dist, rel), text);
  return dist;
}

function seedDir(root, tag, chunks) {
  const dir = path.join(root, 'seeds', tag);
  for (const [rel, text] of Object.entries(chunks)) write(path.join(dir, rel), text);
  write(path.join(dir, '.digest'), `sha256:${'0'.repeat(64)}\n`);
  return path.join(root, 'seeds');
}

test('assemble: the first build holds its export, the seeds, sw.js and compressed siblings', () => {
  const root = tmp();
  const sw = path.join(root, 'sw.js');
  write(sw, 'self.registration.unregister();');
  const dist = exportDir(root, 'dist', {
    '_expo/static/js/web/entry-11.js': 'entry one'.repeat(50),
    '_expo/static/css/a-11.css': 'body{}',
    'assets/assets/images/logo.11.png': 'png',
  });
  const seeds = seedDir(root, '3de3f5d8771b', {
    '_expo/static/js/web/entry-00.js': 'entry zero',
    '_expo/static/js/web/reply-00.js': 'reply zero',
    // a chunk both builds produced, identical
    '_expo/static/css/a-11.css': 'body{}',
  });
  seedDir(root, 'a684b2ebd5b5', {});
  const out = path.join(root, 'out');
  const now = new Date('2026-11-02T10:00:00Z');
  const result = assemble({
    dist,
    sw,
    seedsDir: seeds,
    seeds: [
      { tag: 'a684b2ebd5b5', commit: sha(1) },
      { tag: '3de3f5d8771b', commit: sha(2) },
    ],
    commit: sha(3),
    now,
    out,
  });
  const files = walk(path.join(out, 'app'));
  for (const rel of ['index.html', 'sw.js', '_expo/static/js/web/entry-00.js', '_expo/static/js/web/reply-00.js', '_expo/static/js/web/entry-11.js', 'assets/assets/images/logo.11.png']) {
    assert.ok(files.includes(rel), rel);
  }
  const entry = path.join(out, 'app', '_expo/static/js/web/entry-11.js');
  assert.equal(brotliDecompressSync(readFileSync(`${entry}.br`)).toString(), readFileSync(entry, 'utf8'));
  assert.equal(gunzipSync(readFileSync(`${entry}.gz`)).toString(), readFileSync(entry, 'utf8'));
  assert.ok(existsSync(path.join(out, 'app', 'index.html.br')));
  assert.ok(!existsSync(path.join(out, 'app', 'assets/assets/images/logo.11.png.br')));
  const history = JSON.parse(readFileSync(path.join(out, 'meta', 'assets-history.json'), 'utf8'));
  assert.equal(history['_expo/static/js/web/entry-00.js'], now.toISOString());
  assert.ok(!('index.html' in history));
  assert.equal(result.seeds, 2);
  assert.equal(result.copied, 2);
  // verify-seeds agrees
  const verify = spawnSync(process.execPath, [path.join(here, '..', 'verify-seeds.mjs'), path.join(out, 'app'), seeds, '3de3f5d8771b', 'a684b2ebd5b5'], { encoding: 'utf8' });
  assert.equal(verify.status, 0, verify.stdout + verify.stderr);
  rmSync(path.join(out, 'app', '_expo/static/js/web/reply-00.js'));
  const broken = spawnSync(process.execPath, [path.join(here, '..', 'verify-seeds.mjs'), path.join(out, 'app'), seeds, '3de3f5d8771b'], { encoding: 'utf8' });
  assert.equal(broken.status, 1);
  assert.match(broken.stdout, /missing _expo\/static\/js\/web\/reply-00.js/);
  rmSync(root, { recursive: true, force: true });
});

test('assemble: earlier builds stay for the last 5 builds or 30 days; seeds stay while listed and with the builds that listed them', () => {
  const root = tmp();
  const sw = path.join(root, 'sw.js');
  write(sw, 'sw');
  const seeds = seedDir(root, '3de3f5d8771b', { '_expo/static/js/web/seed-00.js': 'seed' });
  let previous = null;
  const start = new Date('2026-11-01T00:00:00Z').getTime();
  const outs = [];
  // builds 1..8, one a day; the seed is listed by builds 1-2 only
  for (let n = 1; n <= 8; n++) {
    const dist = exportDir(root, `dist${n}`, { [`_expo/static/js/web/entry-${n}.js`]: `entry ${n}`, '_expo/static/js/web/shared-0.js': 'shared' });
    const out = path.join(root, `out${n}`);
    assemble({
      dist,
      sw,
      previous,
      seedsDir: seeds,
      seeds: n <= 2 ? [{ tag: '3de3f5d8771b', commit: sha(99) }] : [],
      commit: sha(n),
      now: new Date(start + n * DAY),
      out,
    });
    // the next build reads this image's filesystem
    previous = out;
    outs.push(out);
  }
  // within 30 days every build is kept
  let files = walk(path.join(previous, 'app'));
  for (let n = 1; n <= 8; n++) assert.ok(files.includes(`_expo/static/js/web/entry-${n}.js`), `entry ${n}`);
  assert.ok(files.includes('_expo/static/js/web/seed-00.js'));
  const history = JSON.parse(readFileSync(path.join(previous, 'meta', 'assets-history.json'), 'utf8'));
  assert.equal(history['_expo/static/js/web/shared-0.js'], new Date(start + DAY).toISOString(), 'first seen is kept');
  // 40 days later: only the last 5 builds; the seed went with builds 1-2
  const dist = exportDir(root, 'dist9', { '_expo/static/js/web/entry-9.js': 'entry 9' });
  const out = path.join(root, 'out9');
  assemble({ dist, sw, previous, commit: sha(9), now: new Date(start + 48 * DAY), out });
  files = walk(path.join(out, 'app'));
  for (let n = 1; n <= 4; n++) assert.ok(!files.includes(`_expo/static/js/web/entry-${n}.js`), `entry ${n} pruned`);
  for (let n = 5; n <= 9; n++) assert.ok(files.includes(`_expo/static/js/web/entry-${n}.js`), `entry ${n} kept`);
  assert.ok(!files.includes('_expo/static/js/web/seed-00.js'));
  assert.ok(!files.includes('_expo/static/js/web/entry-1.js.br'));
  const builds = JSON.parse(readFileSync(path.join(out, 'meta', 'builds.json'), 'utf8'));
  assert.deepEqual(builds.map((build) => build.commit), [sha(9), sha(8), sha(7), sha(6), sha(5)]);
  assert.deepEqual(JSON.parse(readFileSync(path.join(out, 'meta', 'seeds.json'), 'utf8')), {});
  rmSync(root, { recursive: true, force: true });
});

test('assemble: a seed listed by a kept build is restored from the previous image after the seed leaves the list', () => {
  const root = tmp();
  const sw = path.join(root, 'sw.js');
  write(sw, 'sw');
  const seeds = seedDir(root, '3de3f5d8771b', { '_expo/static/js/web/seed-00.js': 'seed' });
  const first = path.join(root, 'out1');
  assemble({ dist: exportDir(root, 'd1', { '_expo/static/js/web/e-1.js': '1' }), sw, seedsDir: seeds, seeds: [{ tag: '3de3f5d8771b', commit: sha(99) }], commit: sha(1), now: new Date('2026-11-02T00:00:00Z'), out: first });
  rmSync(path.join(root, 'seeds'), { recursive: true });
  const second = path.join(root, 'out2');
  assemble({ dist: exportDir(root, 'd2', { '_expo/static/js/web/e-2.js': '2' }), sw, previous: first, commit: sha(2), now: new Date('2026-11-03T00:00:00Z'), out: second });
  assert.equal(readFileSync(path.join(second, 'app', '_expo/static/js/web/seed-00.js'), 'utf8'), 'seed');
  assert.ok(existsSync(path.join(second, 'app', '_expo/static/js/web/seed-00.js.br')));
  rmSync(root, { recursive: true, force: true });
});

test('assemble: a hashed name with different bytes in two kept sources fails', () => {
  const root = tmp();
  const sw = path.join(root, 'sw.js');
  write(sw, 'sw');
  const seeds = seedDir(root, '3de3f5d8771b', { '_expo/static/js/web/entry-11.js': 'other bytes' });
  const dist = exportDir(root, 'dist', { '_expo/static/js/web/entry-11.js': 'entry' });
  assert.throws(
    () => assemble({ dist, sw, seedsDir: seeds, seeds: [{ tag: '3de3f5d8771b', commit: sha(2) }], commit: sha(3), out: path.join(root, 'out') }),
    /entry-11\.js: .* in this build .*; .* in yyc-ai-ui:3de3f5d8771b/,
  );
  rmSync(root, { recursive: true, force: true });
});

test('assemble: a seed that was not fetched, or a previous image without its manifest, fails', () => {
  const root = tmp();
  const sw = path.join(root, 'sw.js');
  write(sw, 'sw');
  const dist = exportDir(root, 'dist', { '_expo/static/js/web/entry-11.js': 'entry' });
  mkdirSync(path.join(root, 'seeds'), { recursive: true });
  assert.throws(() => assemble({ dist, sw, seedsDir: path.join(root, 'seeds'), seeds: [{ tag: '3de3f5d8771b', commit: sha(2) }], commit: sha(3), out: path.join(root, 'out') }), /not fetched/);
  mkdirSync(path.join(root, 'prev', 'app'), { recursive: true });
  assert.throws(() => assemble({ dist, sw, previous: path.join(root, 'prev'), commit: sha(3), out: path.join(root, 'out') }), /no meta\/builds\.json/);
  rmSync(root, { recursive: true, force: true });
});
