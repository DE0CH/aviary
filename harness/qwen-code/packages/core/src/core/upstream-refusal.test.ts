/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, expect, it } from 'vitest';
import type { Content, Part } from '@google/genai';
import {
  MAX_HISTORY_MEDIA_STEPS,
  RefusalRollback,
  mediaFingerprint,
  rewriteRefusedParts,
  upstreamRefusalReason,
} from './upstream-refusal.js';

const REASON = 'Upstream error from DeepInfra: Could not open video stream';
const video = (name: string, data = `data-${name}`): Part => ({
  inlineData: { mimeType: 'video/mp4', data, displayName: name },
});
const toolResult = (id: string, output: string, ...media: Part[]): Part =>
  ({
    functionResponse: {
      id,
      name: 'read_file',
      response: { output },
      ...(media.length > 0 ? { parts: media } : {}),
    },
  }) as Part;
const user = (...parts: Part[]): Content => ({ role: 'user', parts });
const model = (text: string): Content => ({ role: 'model', parts: [{ text }] });
const texts = (content: Content): string => JSON.stringify(content.parts);

describe('upstreamRefusalReason', () => {
  it.each([400, 404, 413, 415, 422])('takes HTTP %i as a refusal', (status) => {
    expect(
      upstreamRefusalReason(Object.assign(new Error(REASON), { status })),
    ).toBe(REASON);
  });

  it('finds the status of an error raised inside the stream, under a cause', () => {
    const inner = Object.assign(new Error(REASON), { status: 400 });
    expect(
      upstreamRefusalReason(new Error('stream failed', { cause: inner })),
    ).toContain(REASON);
  });

  it.each([401, 403, 408, 429, 500, 502, 503, 529])(
    'leaves HTTP %i to the retries that already exist',
    (status) => {
      expect(
        upstreamRefusalReason(Object.assign(new Error('x'), { status })),
      ).toBeUndefined();
    },
  );

  it('leaves an error with no status alone (a dropped connection)', () => {
    expect(upstreamRefusalReason(new Error('socket hang up'))).toBeUndefined();
  });
});

describe('rewriteRefusedParts', () => {
  it('replaces a named video inside a tool result and keeps the call paired', () => {
    const bad = video('clip.mp4');
    const parts = [toolResult('call-1', '', bad), toolResult('call-2', 'ls')];
    const out = rewriteRefusedParts(parts, {
      media: new Set([mediaFingerprint(bad)!]),
      callIds: new Set(),
      reason: REASON,
    })!;
    expect(out.replaced).toBe(1);
    expect(out.parts[0].functionResponse?.id).toBe('call-1');
    const nested = out.parts[0].functionResponse?.parts as Part[];
    expect(nested).toHaveLength(1);
    expect(nested[0].text).toContain('video "clip.mp4" (video/mp4)');
    expect(nested[0].text).toContain(REASON);
    expect(out.parts[1]).toBe(parts[1]);
    // the caller's parts are untouched: the transcript keeps the refused data
    expect(parts[0].functionResponse?.parts?.[0]).toBe(bad);
  });

  it('replaces a whole tool result by its call id', () => {
    const out = rewriteRefusedParts([toolResult('call-1', 'huge output')], {
      media: new Set(),
      callIds: new Set(['call-1']),
      reason: REASON,
    })!;
    const response = out.parts[0].functionResponse!;
    expect(response.id).toBe('call-1');
    expect(response.name).toBe('read_file');
    expect(response.parts).toBeUndefined();
    expect((response.response as { error: string }).error).toContain(REASON);
  });

  it('answers undefined when the plan names nothing here', () => {
    expect(
      rewriteRefusedParts([{ text: 'hello' }, toolResult('c', 'x')], {
        media: new Set(['inline:video/mp4:1:a:a']),
        callIds: new Set(['other']),
        reason: REASON,
      }),
    ).toBeUndefined();
  });
});

describe('RefusalRollback', () => {
  it('blames the media of the delta first, then its results, then earlier media', () => {
    const old = user(toolResult('old', '', video('old.mp4')));
    const delta = user(
      toolResult('new', '', video('new.mp4')),
      toolResult('ls', 'a.txt'),
    );
    const history = [
      user({ text: 'hi' }),
      model('ok'),
      old,
      model('ok'),
      delta,
    ];
    const rollback = new RefusalRollback(() => history, delta);

    expect(rollback.next(REASON)?.plan.stage).toBe('delta-media');
    expect(texts(delta)).not.toContain('data-new.mp4');
    expect(texts(delta)).toContain('a.txt');
    expect(texts(old)).toContain('data-old.mp4');

    expect(rollback.next(REASON)?.plan.stage).toBe('delta-results');
    expect(texts(delta)).not.toContain('a.txt');
    expect(delta.parts?.map((p) => p.functionResponse?.id)).toEqual([
      'new',
      'ls',
    ]);

    expect(rollback.next(REASON)?.plan.stage).toBe('history-media');
    expect(texts(old)).not.toContain('data-old.mp4');

    expect(rollback.next(REASON)).toBeUndefined();
    expect(rollback.accept().map((plan) => plan.stage)).toEqual([
      'delta-media',
      'delta-results',
      'history-media',
    ]);
  });

  it('goes straight to earlier media when the delta is only words ("continue")', () => {
    const poisoned = user(toolResult('bad', '', video('half.mp4')));
    const delta = user({ text: 'continue' });
    const history = [poisoned, delta];
    const rollback = new RefusalRollback(() => history, delta);
    const step = rollback.next(REASON);
    expect(step?.plan.stage).toBe('history-media');
    expect(texts(poisoned)).toContain('Could not open video stream');
    expect(texts(delta)).toBe(JSON.stringify([{ text: 'continue' }]));
  });

  it('gives everything back when no stage was accepted', () => {
    const poisoned = user(toolResult('bad', '', video('half.mp4')));
    const delta = user(toolResult('new', 'out', video('new.mp4')));
    const before = [texts(poisoned), texts(delta)];
    const rollback = new RefusalRollback(() => [poisoned, delta], delta);
    while (rollback.next(REASON));
    rollback.giveUp();
    expect([texts(poisoned), texts(delta)]).toEqual(before);
    expect(rollback.active).toBe(false);
  });

  it('tries a bounded number of earlier entries', () => {
    const history: Content[] = [];
    for (let i = 0; i < MAX_HISTORY_MEDIA_STEPS + 3; i++) {
      history.push(user(toolResult(`c${i}`, '', video(`v${i}.mp4`))));
    }
    const delta = user({ text: 'continue' });
    history.push(delta);
    const rollback = new RefusalRollback(() => history, delta);
    let steps = 0;
    while (rollback.next(REASON)) steps++;
    expect(steps).toBe(MAX_HISTORY_MEDIA_STEPS);
    // newest first: the oldest entries were never touched
    expect(texts(history[0])).toContain('data-v0.mp4');
  });
});
