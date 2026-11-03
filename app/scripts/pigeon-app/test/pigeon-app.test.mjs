// Tests of the pigeon-app bundle scripts (.github/workflows/pigeon-app.yml): node --test scripts/pigeon-app/test/
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { brotliDecompressSync, gunzipSync } from 'node:zlib';

import { assemble, diskUsage, projectMandatory, readPins, SEED_HOURS, walk } from '../assemble.mjs';
import { missingClients } from '../clients-missing.mjs';
import { cachedCopyValid, takeHashedTrees } from '../fetch-seeds.mjs';
import { pinsInKustomization, servedImages } from '../pins.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
const tmp = () => mkdtempSync(path.join(os.tmpdir(), 'pigeon-app-test-'));
const write = (file, text) => {
  mkdirSync(path.dirname(file), { recursive: true });
  writeFileSync(file, text);
};
const sha = (n) => n.toString(16).padStart(40, 'a');
const DAY = 86400 * 1000;

// ---------- pins

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
  assert.deepEqual(pinsInKustomization(kustomization('488a7736ef2c'), 'jarvis3-chat'), ['52c9b8c498b6']);
});

test('servedImages reads the pigeon-app pins the same way', () => {
  const withApp = (ui, app) => `${kustomization(ui)}  - name: pigeon-app\n    newName: ghcr.io/de0ch/pigeon-app\n    newTag: ${app}\n`.replace('configMapGenerator:\n  - name: jarvis-config\n', '');
  const repo = deployRepo([
    { date: '2026-11-01T00:00:00Z', files: { 'yyc-ai/deploy/kustomization.yaml': withApp('aaaaaaaaaaa1', 'bbbbbbbbbbb1') } },
    { date: '2026-11-03T00:00:00Z', files: { 'yyc-ai/deploy/kustomization.yaml': withApp('aaaaaaaaaaa1', 'bbbbbbbbbbb2') } },
  ]);
  const now = new Date('2026-11-04T00:00:00Z');
  assert.deepEqual(servedImages({ repo, image: 'pigeon-app', now }).map((image) => image.tag), ['bbbbbbbbbbb1', 'bbbbbbbbbbb2']);
  assert.deepEqual(servedImages({ repo, image: 'pigeon-app', now, hours: 12, marginHours: 0 }).map((image) => image.tag), ['bbbbbbbbbbb2']);
  assert.deepEqual(servedImages({ repo, now }).map((image) => image.tag), ['aaaaaaaaaaa1']);
  rmSync(repo, { recursive: true, force: true });
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

test('servedImages: a 24 h margin for push and rollout delays, and dates out of order along first-parent', () => {
  const now = new Date('2026-12-31T00:00:00Z');
  // window start without the margin: 2026-12-01T00:00Z; with it: 2026-11-30T00:00Z
  const repo = deployRepo([
    { date: '2026-11-01T00:00:00Z', files: { 'yyc-ai/deploy/kustomization.yaml': kustomization('aaaaaaaaaaa1') } },
    // replaces aaaaaaaaaaa1 12 h before the window start: aaaaaaaaaaa1 is inside the margin only
    { date: '2026-11-30T12:00:00Z', files: { 'yyc-ai/deploy/kustomization.yaml': kustomization('aaaaaaaaaaa2') } },
    // committed with a clock 40 days behind, after aaaaaaaaaaa2: aaaaaaaaaaa2's pin ends no earlier than its own commit
    // (inside the margin only), and the walk does not stop at this old date
    { date: '2026-10-25T00:00:00Z', files: { 'yyc-ai/deploy/kustomization.yaml': kustomization('aaaaaaaaaaa3') } },
    { date: '2026-12-20T00:00:00Z', files: { 'yyc-ai/deploy/kustomization.yaml': kustomization('aaaaaaaaaaa4') } },
  ]);
  const tags = (opts) => servedImages({ repo, now, ...opts }).map((image) => image.tag);
  assert.deepEqual(tags({}), ['aaaaaaaaaaa3', 'aaaaaaaaaaa1', 'aaaaaaaaaaa2', 'aaaaaaaaaaa4']);
  assert.deepEqual(tags({ marginHours: 0 }), ['aaaaaaaaaaa3', 'aaaaaaaaaaa4']);
  const two = servedImages({ repo, now }).find((image) => image.tag === 'aaaaaaaaaaa2');
  assert.equal(new Date(two.until).toISOString(), '2026-11-30T12:00:00.000Z');
  rmSync(repo, { recursive: true, force: true });
});

test('pins.mjs --require-while-pinned: an empty list fails only while the ref still pins the image', () => {
  const repo = deployRepo([
    { date: '2026-10-01T00:00:00Z', files: { 'yyc-ai/deploy/kustomization.yaml': kustomization('aaaaaaaaaaa1') } },
    // the switch's cleanup removes the ui pin
    { date: '2026-11-01T00:00:00Z', files: { 'yyc-ai/deploy/kustomization.yaml': 'resources: [pigeon.yaml]\n' } },
  ]);
  const run = (now, extra = []) =>
    spawnSync(process.execPath, [path.join(here, '..', 'pins.mjs'), '--repo', repo, '--now', now, '--require-while-pinned', ...extra], { encoding: 'utf8' });
  // within 744 h of the removal the last ui image is still listed
  let result = run('2026-11-20T00:00:00Z');
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /^aaaaaaaaaaa1 2026-10-01T00:00:00\.000Z 2026-11-01T00:00:00\.000Z$/m);
  // more than 744 h after it: nothing to list, and that is right
  result = run('2026-12-15T00:00:00Z');
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout, '');
  assert.match(result.stderr, /not pinned at HEAD/);
  // as the repository stood before the removal: pinned now
  result = run('2026-12-15T00:00:00Z', ['--ref', 'HEAD~1']);
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /^aaaaaaaaaaa1 \S+ now$/m);
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
  // files kept only for seeds are served without siblings; a seed file this build also has keeps its siblings
  assert.ok(!existsSync(path.join(out, 'app', '_expo/static/js/web/reply-00.js.br')));
  assert.ok(!existsSync(path.join(out, 'app', '_expo/static/js/web/entry-00.js.gz')));
  assert.ok(existsSync(path.join(out, 'app', '_expo/static/css/a-11.css.br')));
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

test('assemble: earlier builds stay for the last 5 builds, 30 days, or while served; seeds stay while listed', () => {
  const root = tmp();
  const sw = path.join(root, 'sw.js');
  write(sw, 'sw');
  const seeds = seedDir(root, '3de3f5d8771b', { '_expo/static/js/web/seed-00.js': 'seed' });
  let previous = null;
  const start = new Date('2026-11-01T00:00:00Z').getTime();
  // builds 1..8, one a day; the seed is listed for builds 1-2 only
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
    if (n === 2) assert.ok(walk(path.join(out, 'app')).includes('_expo/static/js/web/seed-00.js'));
    // the next build reads this image's filesystem
    previous = out;
  }
  // within 30 days every build is kept, with the siblings of its files; the seed left with its listing
  let files = walk(path.join(previous, 'app'));
  for (let n = 1; n <= 8; n++) {
    assert.ok(files.includes(`_expo/static/js/web/entry-${n}.js`), `entry ${n}`);
    assert.ok(files.includes(`_expo/static/js/web/entry-${n}.js.br`), `entry ${n}.br`);
  }
  assert.ok(!files.includes('_expo/static/js/web/seed-00.js'), 'a seed no longer listed is dropped');
  const history = JSON.parse(readFileSync(path.join(previous, 'meta', 'assets-history.json'), 'utf8'));
  assert.equal(history['_expo/static/js/web/shared-0.js'], new Date(start + DAY).toISOString(), 'first seen is kept');
  // 40 days later: the last 5 builds, and build 2, which YYC still serves (or served within 720 h)
  const dist = exportDir(root, 'dist9', { '_expo/static/js/web/entry-9.js': 'entry 9' });
  const out = path.join(root, 'out9');
  assemble({ dist, sw, previous, servedBuilds: [sha(2)], commit: sha(9), now: new Date(start + 48 * DAY), out });
  files = walk(path.join(out, 'app'));
  for (const n of [1, 3, 4]) assert.ok(!files.includes(`_expo/static/js/web/entry-${n}.js`), `entry ${n} pruned`);
  assert.ok(!files.includes('_expo/static/js/web/entry-1.js.br'));
  for (const n of [2, 5, 6, 7, 8, 9]) assert.ok(files.includes(`_expo/static/js/web/entry-${n}.js`), `entry ${n} kept`);
  const builds = JSON.parse(readFileSync(path.join(out, 'meta', 'builds.json'), 'utf8'));
  assert.deepEqual(builds.map((build) => build.commit), [sha(9), sha(8), sha(7), sha(6), sha(5), sha(2)]);
  assert.deepEqual(Object.keys(builds[0]).sort(), ['builtAt', 'commit', 'files']);
  assert.deepEqual(JSON.parse(readFileSync(path.join(out, 'meta', 'seeds.json'), 'utf8')), {});
  // a served build the previous image no longer holds cannot be restored: an error, not a silent gap
  assert.throws(
    () => assemble({ dist, sw, previous: out, servedBuilds: [sha(3)], commit: sha(10), now: new Date(start + 49 * DAY), out: path.join(root, 'out10') }),
    /served within the Access session lifetime but neither in the previous image's builds\.json nor restored \(--restore\): a+3/,
  );
  // restored from build 3's own image: its files come back (with siblings), and it is listed again
  const restored = assemble({
    dist,
    sw,
    previous: out,
    servedBuilds: [sha(3)],
    restore: [{ commit: sha(3), dir: path.join(root, 'out3') }],
    commit: sha(10),
    now: new Date(start + 49 * DAY),
    out: path.join(root, 'out10'),
  });
  files = walk(path.join(root, 'out10', 'app'));
  assert.ok(files.includes('_expo/static/js/web/entry-3.js') && files.includes('_expo/static/js/web/entry-3.js.br'));
  assert.equal(readFileSync(path.join(root, 'out10', 'app', '_expo/static/js/web/entry-3.js'), 'utf8'), 'entry 3');
  const after = JSON.parse(readFileSync(path.join(root, 'out10', 'meta', 'builds.json'), 'utf8'));
  assert.ok(after.some((build) => build.commit === sha(3)));
  assert.equal(restored.pruned.length, 0);
  assert.throws(
    () => assemble({ dist, sw, previous: out, restore: [{ commit: sha(4), dir: path.join(root, 'out3') }], commit: sha(10), out: path.join(root, 'out11') }),
    /does not list that build/,
  );
  rmSync(root, { recursive: true, force: true });
});

test('assemble: a re-pushed ui tag keeps the files of its earlier digest from the previous image', () => {
  const root = tmp();
  const sw = path.join(root, 'sw.js');
  write(sw, 'sw');
  const tag = '3de3f5d8771b';
  const seeds = seedDir(root, tag, { '_expo/static/js/web/lazy-aa.js': 'first export' });
  const first = path.join(root, 'out1');
  assemble({ dist: exportDir(root, 'd1', { '_expo/static/js/web/e-1.js': '1' }), sw, seedsDir: seeds, seeds: [{ tag, commit: sha(99) }], commit: sha(1), now: new Date('2026-11-02T00:00:00Z'), out: first });
  // jarvis-image re-run for the same commit: a new digest, a different export
  rmSync(path.join(seeds, tag), { recursive: true });
  seedDir(root, tag, { '_expo/static/js/web/lazy-bb.js': 'second export' });
  write(path.join(seeds, tag, '.digest'), `sha256:${'1'.repeat(64)}\n`);
  const second = path.join(root, 'out2');
  assemble({ dist: exportDir(root, 'd2', { '_expo/static/js/web/e-2.js': '2' }), sw, previous: first, seedsDir: seeds, seeds: [{ tag, commit: sha(99) }], commit: sha(2), now: new Date('2026-11-03T00:00:00Z'), out: second });
  assert.equal(readFileSync(path.join(second, 'app', '_expo/static/js/web/lazy-aa.js'), 'utf8'), 'first export');
  assert.equal(readFileSync(path.join(second, 'app', '_expo/static/js/web/lazy-bb.js'), 'utf8'), 'second export');
  const recorded = JSON.parse(readFileSync(path.join(second, 'meta', 'seeds.json'), 'utf8'))[tag];
  assert.equal(recorded.digest, `sha256:${'1'.repeat(64)}`);
  assert.deepEqual(recorded.older.map((older) => older.digest), [`sha256:${'0'.repeat(64)}`]);
  // the earlier digest's file altered in the previous image: refused
  write(path.join(second, 'app', '_expo/static/js/web/lazy-aa.js'), 'tampered');
  assert.throws(
    () => assemble({ dist: exportDir(root, 'd3', { '_expo/static/js/web/e-3.js': '3' }), sw, previous: second, seedsDir: seeds, seeds: [{ tag, commit: sha(99) }], commit: sha(3), out: path.join(root, 'out3') }),
    /lazy-aa\.js: yyc-ai-ui:3de3f5d8771b@sha256:0+ recorded/,
  );
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

test('assemble: over the budget it fails and names what takes the space', () => {
  const root = tmp();
  const sw = path.join(root, 'sw.js');
  write(sw, 'sw');
  const seeds = seedDir(root, '3de3f5d8771b', { '_expo/static/js/web/big-00.js': 'x'.repeat(300000) });
  const dist = exportDir(root, 'dist', { '_expo/static/js/web/entry-11.js': 'entry' });
  const args = { dist, sw, seedsDir: seeds, seeds: [{ tag: '3de3f5d8771b', commit: sha(2) }], commit: sha(3), out: path.join(root, 'out') };
  const ok = assemble({ ...args, budgetBytes: 1048576 });
  assert.equal(ok.bytes, diskUsage(path.join(root, 'out', 'app')));
  // 300000 bytes in 4 KiB blocks, no siblings (a seed-only file)
  assert.equal(ok.usage.find((entry) => entry.label === 'yyc-ai-ui:3de3f5d8771b').bytes, 74 * 4096);
  assert.throws(() => assemble({ ...args, budgetBytes: 200000 }), (error) => {
    assert.match(error.message, /over the 0\.2 MiB budget/);
    assert.match(error.message, /0\.3 MiB {2}yyc-ai-ui:3de3f5d8771b/);
    return true;
  });
  rmSync(root, { recursive: true, force: true });
});

// The first image as pushed (sizes of every file of pigeon-app:0b3a5c6f5028, its build and its 39 seeds), then 7
// more builds a day apart, all within 30 days, each with every file changed (what two real builds, 0b3a5c6f and
// 140ed041, come close to: 58 of 82 files differ). Seeds get incompressible filler (they are only copied); the builds'
// files get text that brotli and gzip shrink about as much as the real chunks (0.19 and 0.22 of their size, against
// 0.19 and 0.23 for the real build). YYC serves build 2 from build 3 on. Every image must fit the budget, by pruning
// the builds YYC has not served, oldest first.
test('assemble at the real sizes: 8 builds within 30 days, every file changed each time, all fit the budget', { timeout: 1200000 }, () => {
  const BUDGET = 480 * 1048576;
  const N = 8;
  const inventory = JSON.parse(readFileSync(path.join(here, 'sizes-0b3a5c6f5028.json'), 'utf8'));
  const root = tmp();
  const sw = path.join(root, 'sw.js');
  write(sw, 'self.registration.unregister();');
  const words = 'const let function return this props state value index chunk module export import default async await undefined null true false object string number length children className style onPress useState useEffect'.split(' ');
  const text = (size, seed) => {
    const parts = [];
    let length = 0;
    for (let i = 0; length < size; i++) {
      const r = createHash('sha256').update(`${seed}:${i}`).digest();
      for (let k = 0; k + 3 < r.length && length < size; k += 3) {
        const word = words[r[k] % words.length] + (r[k + 1] < 60 ? `_${r[k + 2].toString(36)}` : '') + (r[k + 2] % 4 ? ' ' : ';');
        parts.push(word);
        length += word.length;
      }
    }
    return parts.join('').slice(0, size);
  };
  const filler = (size, seed) => {
    const block = createHash('sha512').update(seed).digest();
    const buffer = Buffer.alloc(size);
    for (let i = 0; i < size; i += block.length) block.copy(buffer, i);
    return buffer;
  };
  const [names, sizes] = [inventory.files.map(([name]) => name), inventory.files.map(([, size]) => size)];
  const seedsDir = path.join(root, 'seeds');
  const seeds = [];
  for (const [tag, seed] of Object.entries(inventory.seeds)) {
    for (const i of seed.files) write(path.join(seedsDir, tag, names[i]), inventory.build.files.includes(i) ? text(sizes[i], names[i]) : filler(sizes[i], names[i]));
    write(path.join(seedsDir, tag, '.digest'), `sha256:${createHash('sha256').update(tag).digest('hex')}\n`);
    seeds.push({ tag, commit: seed.commit });
  }
  const commits = [inventory.build.commit, ...Array.from({ length: N - 1 }, (_, i) => sha(100 + i))];
  const start = new Date('2026-11-02T15:50:00Z').getTime();
  const rename = (name, n) => (n === 0 ? name : name.replace(/[0-9a-f]{32}/, (hash) => createHash('md5').update(`${hash}:${n}`).digest('hex')));
  let previous = null;
  const results = [];
  for (let n = 0; n < N; n++) {
    const dist = path.join(root, `dist${n}`);
    for (const i of inventory.build.files) write(path.join(dist, rename(names[i], n)), text(sizes[i], rename(names[i], n)));
    write(path.join(dist, 'index.html'), `<html>${n}</html>`);
    const out = path.join(root, `out${n}`);
    const result = assemble({
      dist,
      sw,
      previous,
      seedsDir,
      seeds,
      servedBuilds: n >= 2 ? [commits[1]] : [],
      commit: commits[n],
      budgetBytes: BUDGET,
      now: new Date(start + n * DAY),
      out,
    });
    results.push(result);
    console.log(`build ${n + 1}: ${(result.bytes / 1048576).toFixed(1)} MiB, ${result.builds} builds kept, pruned ${result.pruned.map((c) => c.slice(0, 12)).join(' ') || 'none'}`);
    assert.ok(result.bytes <= BUDGET, `build ${n + 1}: ${(result.bytes / 1048576).toFixed(1)} MiB`);
    assert.equal(result.bytes, diskUsage(path.join(out, 'app')));
    const kept = JSON.parse(readFileSync(path.join(out, 'meta', 'builds.json'), 'utf8')).map((build) => build.commit);
    // this build, the one before it, and the served one always stay
    assert.equal(kept[0], commits[n]);
    if (n >= 1) assert.equal(kept[1], commits[n - 1]);
    if (n >= 2) assert.ok(kept.includes(commits[1]), `build ${n + 1} keeps the served build 2`);
    // every kept build's files are there, with siblings; every seed's files too
    const files = new Set(walk(path.join(out, 'app')));
    for (const i of inventory.build.files) {
      for (const k of kept.map((c) => commits.indexOf(c))) {
        const rel = rename(names[i], k);
        assert.ok(files.has(rel) && (!/\.(js|css|json|svg|wasm)$/.test(rel) || files.has(`${rel}.br`)), `build ${n + 1}: build ${k + 1}'s ${rel}`);
      }
    }
    const verify = spawnSync(process.execPath, [path.join(here, '..', 'verify-seeds.mjs'), path.join(out, 'app'), seedsDir, ...seeds.map((seed) => seed.tag)], { encoding: 'utf8' });
    assert.equal(verify.status, 0, verify.stdout);
    if (previous) rmSync(previous, { recursive: true, force: true });
    rmSync(dist, { recursive: true, force: true });
    previous = out;
  }
  // a build whose every file changed adds about 36 MiB, so the budget holds 5 such builds with all 39 seeds; past
  // that, the builds YYC has not served are pruned oldest first (build 1's files stay as a seed's, so pruning it
  // frees only their siblings), and every later image still keeps at least 4 builds
  const perBuild = results[1].bytes - results[0].bytes;
  assert.ok(perBuild > 30 * 1048576 && perBuild < 42 * 1048576, `per build ${(perBuild / 1048576).toFixed(1)} MiB`);
  assert.deepEqual(results.slice(0, 5).map((result) => result.builds), [1, 2, 3, 4, 5]);
  for (const result of results.slice(5)) assert.ok(result.builds >= 4, `${result.builds} builds kept`);
  const prunedInOrder = results.flatMap((result) => result.pruned);
  assert.ok(prunedInOrder.length > 0);
  assert.deepEqual(prunedInOrder, commits.filter((c) => c !== commits[1]).slice(0, prunedInOrder.length), 'oldest unserved first');
  rmSync(root, { recursive: true, force: true });
});

const MIB = 1048576;
const HOUR = 3600 * 1000;

test('projectMandatory: seeds leave 744 h after their last serving while new pins add their growth', () => {
  const now = new Date('2026-11-03T00:00:00Z');
  const t0 = now.getTime();
  const result = projectMandatory({
    fixedBytes: 100 * MIB,
    seedFiles: [
      { bytes: 260 * MIB, expires: t0 + 26 * DAY },
      // beyond the horizon: stays throughout
      { bytes: 50 * MIB, expires: t0 + 40 * DAY },
    ],
    growthBytesPerDay: 11 * MIB,
    budgetBytes: 480 * MIB,
    now,
  });
  assert.equal(result.now, 410 * MIB);
  // 410 + 11 MiB a day passes 480 after 6.36 days: the first hourly sample over it is hour 153
  assert.equal(result.overAt, t0 + 153 * HOUR);
  // the old seeds leave at day 26: 100 + 50 + 286 fits again
  assert.equal(result.fitsAgainAt, t0 + 26 * DAY);
  assert.equal(result.peak.at, t0 + (26 * 24 - 1) * HOUR);
  assert.equal(result.end.at, t0 + SEED_HOURS * HOUR);
  // no growth and room to spare: never over
  const calm = projectMandatory({ fixedBytes: 100 * MIB, seedFiles: [{ bytes: 260 * MIB, expires: t0 + DAY }], growthBytesPerDay: 0, budgetBytes: 480 * MIB, now });
  assert.equal(calm.overAt, null);
  assert.equal(calm.fitsAgainAt, null);
  assert.equal(calm.end.bytes, 100 * MIB);
});

// The seed growth Claude measured on 2026-11-03 from pigeon-app:main's meta (built from 0079b2fc): the ui pins since
// the chunk split (2026-10-29) added 25.2, 24.8, 0, 5.9, 0 and 0 MiB of new files, 55.9 MiB in 5 days, and the
// pre-split seeds stay until 2026-11-29 11:44 (the last pre-split pin ended 2026-10-29 11:44, plus 744 h). With the
// first image's real seed sizes as the pre-split part and two builds (36 MiB each with siblings) as the fixed part,
// the mandatory part goes over 480 MiB in mid-November and fits again only once the pre-split seeds leave.
test('projectMandatory at the measured seed growth: over the 480 MiB budget from mid-November until the pre-split seeds leave', () => {
  const inventory = JSON.parse(readFileSync(path.join(here, 'sizes-0b3a5c6f5028.json'), 'utf8'));
  const blocks = (bytes) => Math.max(1, Math.ceil(bytes / 4096)) * 4096;
  const build = new Set(inventory.build.files);
  const seedOnly = new Set(Object.values(inventory.seeds).flatMap((seed) => seed.files).filter((i) => !build.has(i)));
  const expires = new Date('2026-10-29T11:44:00Z').getTime() + SEED_HOURS * HOUR;
  const now = new Date('2026-11-03T00:00:00Z');
  const result = projectMandatory({
    fixedBytes: 2 * 36 * MIB,
    seedFiles: [...seedOnly].map((i) => ({ bytes: blocks(inventory.files[i][1]), expires })),
    growthBytesPerDay: (55.9 * MIB) / 5,
    budgetBytes: 480 * MIB,
    now,
  });
  console.log(
    `now ${(result.now / MIB).toFixed(1)} MiB, over from ${new Date(result.overAt).toISOString()}, peak ${(result.peak.bytes / MIB).toFixed(1)} MiB at ` +
      `${new Date(result.peak.at).toISOString()}, fits again ${new Date(result.fitsAgainAt).toISOString()}`,
  );
  assert.ok(result.overAt > new Date('2026-11-10T00:00:00Z').getTime() && result.overAt < new Date('2026-11-20T00:00:00Z').getTime());
  assert.equal(result.fitsAgainAt, new Date('2026-11-29T12:00:00Z').getTime());
  assert.ok(result.peak.bytes > 600 * MIB && result.peak.bytes < 700 * MIB, `peak ${(result.peak.bytes / MIB).toFixed(1)} MiB`);
});

test('assemble reports the mandatory part and, with the pins, projects it: a shared file leaves with its newest seed', () => {
  const root = tmp();
  const sw = path.join(root, 'sw.js');
  write(sw, 'sw');
  const kib = (n, c) => c.repeat(n * 1024);
  // pinned 20 to 10 days ago, then the recent one from 2 days ago until now
  seedDir(root, 'aaaaaaaaaaa1', { '_expo/static/js/web/old-1.js': kib(400, 'o'), '_expo/static/js/web/shared-1.js': kib(40, 's') });
  const seeds = seedDir(root, 'aaaaaaaaaaa2', { '_expo/static/js/web/new-2.js': kib(140, 'n'), '_expo/static/js/web/shared-1.js': kib(40, 's') });
  const dist = exportDir(root, 'dist', { '_expo/static/js/web/entry-11.js': 'entry' });
  const now = new Date('2026-11-03T00:00:00Z');
  const iso = (days) => new Date(now.getTime() - days * DAY).toISOString();
  const pins = readPins(`aaaaaaaaaaa1 ${iso(20)} ${iso(10)}\naaaaaaaaaaa2 ${iso(2)} now\n`);
  const result = assemble({
    dist,
    sw,
    seedsDir: seeds,
    seeds: [
      { tag: 'aaaaaaaaaaa1', commit: sha(1) },
      { tag: 'aaaaaaaaaaa2', commit: sha(2) },
    ],
    commit: sha(3),
    out: path.join(root, 'out'),
    budgetBytes: 2 * MIB,
    now,
    pins,
  });
  // one build only: everything is mandatory
  assert.equal(result.mandatory.bytes, result.bytes);
  assert.equal(result.mandatory.seedBytes, (400 + 40 + 140) * 1024);
  assert.equal(result.mandatory.headroom, 2 * MIB - result.bytes);
  // growth: the recent pin's own files (new-2.js; shared-1.js the older pin holds already), over 7 days
  assert.equal(result.projection.recentPins, 1);
  assert.equal(result.projection.growthBytesPerDay, (140 * 1024) / 7);
  // old-1.js leaves 744 h after its pin ended (10 days ago): 21 days from now; shared-1.js stays with the recent pin
  const fixed = result.mandatory.bytes - result.mandatory.seedBytes;
  const atDay = (day) => fixed + (40 + 140) * 1024 + (day >= 21 ? 0 : 400 * 1024) + (result.projection.growthBytesPerDay * day);
  assert.equal(result.projection.end.bytes, atDay(31));
  assert.equal(result.projection.peak.at, now.getTime() + (21 * 24 - 1) * HOUR);
  assert.equal(result.projection.overAt, null);
  // the CLI prints both, and warns when the projection goes over
  const cli = spawnSync(
    process.execPath,
    [
      path.join(here, '..', 'assemble.mjs'),
      '--dist', dist, '--sw', sw, '--commit', sha(3), '--out', path.join(root, 'out2'), '--seeds', seeds,
      '--seed', `aaaaaaaaaaa1=${sha(1)}`, '--seed', `aaaaaaaaaaa2=${sha(2)}`, '--budget-mib', '1', '--now', now.toISOString(),
      '--pins', (write(path.join(root, 'pins.txt'), `aaaaaaaaaaa1 ${iso(20)} ${iso(10)}\naaaaaaaaaaa2 ${iso(2)} now\n`), path.join(root, 'pins.txt')),
    ],
    { encoding: 'utf8' },
  );
  assert.equal(cli.status, 0, cli.stderr);
  assert.match(cli.stdout, /mandatory part \(this build, the one before it, the served builds, the seeds\): 0\.6 MiB, of it seeds' own files 0\.6 MiB; headroom 0\.4 MiB/);
  assert.match(cli.stdout, /new seeds add 0\.0 MiB a day, what the 1 ui pins of the last 7 days added\): peak/);
  rmSync(root, { recursive: true, force: true });
});

// ---------- fetch-seeds

test('fetch-seeds trusts a cached seed only at the current digest and with the files the previous image recorded', () => {
  const root = tmp();
  const dir = path.join(root, '3de3f5d8771b');
  const digest = `sha256:${'2'.repeat(64)}`;
  write(path.join(dir, '_expo/static/js/web/a-1.js'), 'a');
  write(path.join(dir, 'assets/x.1.png'), 'png');
  write(path.join(dir, '.digest'), `${digest}\n`);
  const hash = (text) => createHash('sha256').update(text).digest('hex');
  const record = { digest, files: { '_expo/static/js/web/a-1.js': hash('a'), 'assets/x.1.png': hash('png') } };
  assert.deepEqual(cachedCopyValid(dir, digest, record), { ok: true });
  assert.deepEqual(cachedCopyValid(dir, digest, { digest: `sha256:${'3'.repeat(64)}`, files: {}, older: [record] }), { ok: true });
  assert.match(cachedCopyValid(dir, `sha256:${'4'.repeat(64)}`, record).why, /is not the tag's/);
  assert.match(cachedCopyValid(dir, digest, undefined).why, /no trusted manifest/);
  assert.match(cachedCopyValid(path.join(root, 'none'), digest, record).why, /not cached/);
  // a poisoned cache: altered bytes, an added file, a missing file
  write(path.join(dir, '_expo/static/js/web/a-1.js'), 'evil');
  assert.match(cachedCopyValid(dir, digest, record).why, /a-1\.js differs/);
  write(path.join(dir, '_expo/static/js/web/a-1.js'), 'a');
  write(path.join(dir, '_expo/static/js/web/extra-1.js'), 'evil');
  assert.match(cachedCopyValid(dir, digest, record).why, /file list differs/);
  rmSync(path.join(dir, '_expo/static/js/web/extra-1.js'));
  rmSync(path.join(dir, 'assets/x.1.png'));
  assert.match(cachedCopyValid(dir, digest, record).why, /file list differs/);
  rmSync(root, { recursive: true, force: true });
});

test('fetch-seeds keeps a Paseo image\'s hashed trees and nothing of a pre-Paseo one', () => {
  const root = tmp();
  write(path.join(root, 'paseo/app/dist/_expo/static/js/web/a-1.js'), 'a');
  write(path.join(root, 'paseo/app/dist/assets/x.1.png'), 'png');
  write(path.join(root, 'paseo/app/dist/index.html'), 'html');
  takeHashedTrees(path.join(root, 'paseo'), path.join(root, 'out1'));
  assert.deepEqual(walk(path.join(root, 'out1')), ['_expo/static/js/web/a-1.js', 'assets/x.1.png']);
  write(path.join(root, 'old/app/dist/assets/index-1.js'), 'cloudcli');
  write(path.join(root, 'out2/stale.js'), 'stale');
  takeHashedTrees(path.join(root, 'old'), path.join(root, 'out2'));
  assert.deepEqual(walk(path.join(root, 'out2')), []);
  rmSync(root, { recursive: true, force: true });
});

// ---------- check-frames

test('unpack-harness: a harness at the tarball root, in package/, in ./package/ with dotfiles; one without it is refused', () => {
  const root = tmp();
  const script = path.join(here, '..', 'unpack-harness.sh');
  const layouts = {
    root: { dir: 'src', files: { 'validate-frames.mjs': 'v', 'package.json': '{}', 'lib/protocol.mjs': 'p' }, args: ['-C', 'src', '.'] },
    package: { dir: 'src', files: { 'package/validate-frames.mjs': 'v', 'package/package.json': '{}', 'package/lib/protocol.mjs': 'p' }, args: ['-C', 'src', 'package'] },
    dotfiles: { dir: 'src', files: { 'package/validate-frames.mjs': 'v', 'package/.npmrc': 'x', 'package/lib/protocol.mjs': 'p' }, args: ['-C', 'src', './package'] },
  };
  for (const [name, layout] of Object.entries(layouts)) {
    const src = path.join(root, name, layout.dir);
    for (const [rel, text] of Object.entries(layout.files)) write(path.join(src, rel), text);
    const tgz = path.join(root, `${name}.tgz`);
    execFileSync('tar', ['-czf', tgz, '-C', path.join(root, name), ...layout.args]);
    const dest = path.join(root, `${name}-out`);
    const result = spawnSync('bash', [script, tgz, dest], { encoding: 'utf8' });
    assert.equal(result.status, 0, `${name}: ${result.stderr}`);
    assert.equal(readFileSync(path.join(dest, 'validate-frames.mjs'), 'utf8'), 'v', name);
    assert.ok(existsSync(path.join(dest, 'lib/protocol.mjs')), name);
    if (name === 'dotfiles') assert.ok(existsSync(path.join(dest, '.npmrc')));
  }
  write(path.join(root, 'none/src/package.json'), '{}');
  execFileSync('tar', ['-czf', path.join(root, 'none.tgz'), '-C', path.join(root, 'none/src'), '.']);
  const refused = spawnSync('bash', [script, path.join(root, 'none.tgz'), path.join(root, 'none-out')], { encoding: 'utf8' });
  assert.equal(refused.status, 2);
  assert.match(refused.stderr, /no validate-frames\.mjs/);
  rmSync(root, { recursive: true, force: true });
});

test('self-test-verdict: only a run that rejected the seeded frame and nothing else passes', () => {
  const root = tmp();
  const script = path.join(here, '..', 'self-test-verdict.sh');
  const verdict = (status, log) => {
    write(path.join(root, 'log'), log);
    return spawnSync('bash', [script, String(status), path.join(root, 'log')], { encoding: 'utf8' });
  };
  const seeded = '     /w/scripts/pigeon-app/test/incompatible-frame.ndjson:1 out hello.rejected (seeded incompatible frame: hello.rejected with a reason no client knows): reason: invalid enum value\n';
  const rejected = `check-frames: corpus\nFAIL protocol 0123456789 (zod 4.1.0 + zod-aot 0.4.0) of this checkout: 412 server frames, 37 client frames, 1 invalid\n${seeded}`;
  assert.equal(verdict(1, rejected).status, 0, verdict(1, rejected).stdout);
  // a crash under set -e, npm ci failing, node throwing: exit 1 with no verdict
  assert.equal(verdict(1, 'npm ERR! code E404\n').status, 1);
  assert.match(verdict(1, 'npm ERR! code E404\n').stdout, /printed no verdict/);
  assert.match(verdict(0, rejected).stdout, /answered 0/);
  assert.match(verdict(2, rejected).stdout, /answered 2/);
  // another frame rejected as well, or instead
  const other = '     /w/frames/corpus/pigeon-frames.ndjson:9 out session.update: bad\n';
  assert.match(verdict(1, rejected.replace('1 invalid', '2 invalid') + other).stdout, /exactly one frame/);
  assert.match(verdict(1, rejected.replace(seeded, other)).stdout, /other than the seeded one/);
  rmSync(root, { recursive: true, force: true });
});

// ---------- clients-missing

test('clients-missing: the dispatched commits clients.txt does not list yet', () => {
  const listed = `# header ${sha(1)}\n${sha(2)} web-legacy 2026-10-21T14:19:07Z\n${sha(3)} web,ios    2026-10-27T01:06:40Z\n`;
  assert.deepEqual(missingClients(listed, [sha(2), sha(3)]), []);
  assert.deepEqual(missingClients(listed, [sha(1), sha(3), sha(4)]), [sha(1), sha(4)]);
});

test('a1-ready: the A1 image exists, was built from the A1 commit, seeds its own ui image and every ui image served within 744 h', () => {
  const root = tmp();
  // a fake crane: `crane export --platform P IMAGE -` streams a tar of $FAKE_IMAGES/<tag>, or fails like a missing tag
  const bin = path.join(root, 'bin');
  write(
    path.join(bin, 'crane'),
    '#!/usr/bin/env bash\n[ "$1" = export ] || exit 3\ndir="$FAKE_IMAGES/${4##*:}"\n' +
      '[ -d "$dir" ] || { echo "MANIFEST_UNKNOWN: $4" >&2; exit 1; }\ntar -c -C "$dir" meta\n',
  );
  execFileSync('chmod', ['+x', path.join(bin, 'crane')]);
  const images = path.join(root, 'images');
  const image = (commit, builtFrom, seeds) => {
    write(path.join(images, commit.slice(0, 12), 'meta', 'builds.json'), JSON.stringify([{ commit: builtFrom, files: [] }]));
    write(path.join(images, commit.slice(0, 12), 'meta', 'seeds.json'), JSON.stringify(seeds));
  };
  const a1 = '1'.repeat(40);
  const other = '2'.repeat(40);
  const late = '3'.repeat(40);
  const ago = (days) => new Date(Date.now() - days * DAY).toISOString();
  // DE0CH/YYC: `other` pinned 40 to 3 days ago (served within 744 h), an older pin long gone
  const yyc = deployRepo([
    { date: ago(90), files: { 'yyc-ai/deploy/kustomization.yaml': kustomization('999999999999') } },
    { date: ago(40), files: { 'yyc-ai/deploy/kustomization.yaml': kustomization(other.slice(0, 12)) } },
    { date: ago(3), files: { 'yyc-ai/deploy/kustomization.yaml': kustomization(a1.slice(0, 12)) } },
  ]);
  const run = (...args) =>
    spawnSync('bash', [path.join(here, '..', 'a1-ready.sh'), ...args], {
      encoding: 'utf8',
      env: { ...process.env, PATH: `${bin}:${process.env.PATH}`, FAKE_IMAGES: images },
    });
  assert.equal(run('abc', yyc).status, 2);
  assert.equal(run(a1).status, 2);
  let result = run(a1, yyc);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /no ghcr\.io\/de0ch\/pigeon-app:111111111111\. .*re-run that commit's pigeon-app run/);
  image(a1, other, { [a1.slice(0, 12)]: { commit: a1, files: {} } });
  result = run(a1, yyc);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /was built from 2{40}, not 1{40}/);
  image(a1, a1, { [other.slice(0, 12)]: { commit: other, files: {} } });
  result = run(a1, yyc);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /does not seed yyc-ai-ui:111111111111/);
  image(a1, a1, { [other.slice(0, 12)]: { commit: other, files: {} }, [a1.slice(0, 12)]: { commit: a1, files: {} } });
  result = run(a1, yyc);
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /seeds yyc-ai-ui:111111111111 and 1 other ui images, every one served within 744 h \(2 pins\)/);
  // a ui pin that landed after the A1 build read the pins (a rollback to `late`): the A1 image lacks its chunks
  write(path.join(yyc, 'yyc-ai/deploy/kustomization.yaml'), kustomization(late.slice(0, 12)));
  execFileSync('git', ['-C', yyc, '-c', 'user.name=t', '-c', 'user.email=t@example.com', 'commit', '-q', '-am', 'rollback'], { env: { ...process.env, GIT_COMMITTER_NAME: 't', GIT_COMMITTER_EMAIL: 't@example.com' } });
  result = run(a1, yyc);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /does not seed yyc-ai-ui 333333333333, served within 744 h/);
  rmSync(yyc, { recursive: true, force: true });
  rmSync(root, { recursive: true, force: true });
});
