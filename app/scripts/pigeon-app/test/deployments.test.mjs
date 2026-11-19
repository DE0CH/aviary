import assert from 'node:assert/strict';
import { test } from 'node:test';

import { served } from '../deployments.mjs';

const now = new Date('2026-12-01T00:00:00Z');
const opts = { image: 'pigeon-app', hours: 720, marginHours: 24, now };
const d = (created_at, tag) => ({ created_at, tags: tag ? { 'pigeon-app': tag, pigeon: 'x' } : { pigeon: 'x' } });

test('the tag in force serves until now; the one it replaced until the replacement', () => {
  const got = served([d('2026-11-25T00:00:00Z', 'aaaaaaaaaaaa'), d('2026-11-28T00:00:00Z', 'bbbbbbbbbbbb')], opts);
  assert.deepEqual(got, [
    { tag: 'aaaaaaaaaaaa', from: '2026-11-25T00:00:00Z', until: '2026-11-28T00:00:00Z' },
    { tag: 'bbbbbbbbbbbb', from: '2026-11-28T00:00:00Z', until: 'now' },
  ]);
});

test('a redeploy of the same tag does not end its interval, and a tag replaced before the window is left out', () => {
  const got = served([
    d('2026-09-01T00:00:00Z', 'oooooooooooo'),
    d('2026-10-01T00:00:00Z', 'aaaaaaaaaaaa'),
    d('2026-11-20T00:00:00Z', 'aaaaaaaaaaaa'),
    d('2026-11-29T00:00:00Z', 'bbbbbbbbbbbb'),
  ], opts);
  assert.deepEqual(got.map((x) => [x.tag, x.until]), [['aaaaaaaaaaaa', '2026-11-29T00:00:00Z'], ['bbbbbbbbbbbb', 'now']]);
});

test('a deployment that names no tag for the image changes nothing', () => {
  const got = served([d('2026-11-25T00:00:00Z', 'aaaaaaaaaaaa'), d('2026-11-26T00:00:00Z', null)], opts);
  assert.deepEqual(got.map((x) => [x.tag, x.until]), [['aaaaaaaaaaaa', 'now']]);
});
