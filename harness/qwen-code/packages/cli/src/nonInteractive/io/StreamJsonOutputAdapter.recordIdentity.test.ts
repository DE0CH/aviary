/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

// de0ch: a host that shows the answer as it is written (aviary) matches each streamed block to the
// transcript's record by (message id, block index). The stream named a reply by its first part's own
// id and numbered every block 0, so the finished record never met its streamed text and the answer
// stood twice in the chat.

import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import type { Config } from '@qwen-code/qwen-code-core';
import { LlmEventType } from '@qwen-code/qwen-code-core';
import { StreamJsonOutputAdapter } from './StreamJsonOutputAdapter.js';

interface Recording {
  reserveAssistantRecordUuid: ReturnType<typeof vi.fn>;
  releaseAssistantRecordUuid: ReturnType<typeof vi.fn>;
}

function setup(): {
  adapter: StreamJsonOutputAdapter;
  recording: Recording;
  events: () => Array<{ type: string; index?: number; message?: { id: string } }>;
} {
  let n = 0;
  const recording: Recording = {
    reserveAssistantRecordUuid: vi.fn(() => `record-${++n}`),
    releaseAssistantRecordUuid: vi.fn(),
  };
  const config = {
    getSessionId: vi.fn().mockReturnValue('test-session-id'),
    getModel: vi.fn().mockReturnValue('test-model'),
    getChatRecordingService: vi.fn().mockReturnValue(recording),
  } as unknown as Config;
  const lines: string[] = [];
  const out = {
    write: (chunk: string) => {
      lines.push(chunk);
      return true;
    },
  } as unknown as NodeJS.WritableStream;
  const adapter = new StreamJsonOutputAdapter(config, true, out);
  return {
    adapter,
    recording,
    events: () =>
      lines
        .map((l) => JSON.parse(l))
        .filter((m) => m.type === 'stream_event')
        .map((m) => m.event),
  };
}

describe('StreamJsonOutputAdapter: the stream names what the transcript records', () => {
  beforeEach(() => {
    vi.spyOn(process.stdout, 'write').mockImplementation(() => true);
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("names a reply by its record's uuid and numbers its blocks as the record's parts", () => {
    const { adapter, recording, events } = setup();
    adapter.startAssistantMessage();
    adapter.processEvent({
      type: LlmEventType.Thought,
      value: { subject: '', description: 'thinking' },
    } as never);
    adapter.processEvent({ type: LlmEventType.Content, value: 'the answer' });
    adapter.processEvent({
      type: LlmEventType.ToolCallRequest,
      value: {
        callId: 'call-1',
        name: 'run_shell_command',
        args: { command: 'true' },
        isClientInitiated: false,
        prompt_id: 'p',
      },
    } as never);
    adapter.finalizeAssistantMessage();

    const ev = events();
    const starts = ev.filter((e) => e.type === 'message_start');
    expect(starts).toHaveLength(1);
    expect(starts[0].message?.id).toBe('record-1');
    // thought, text, tool call: the record's parts 0, 1, 2
    expect(
      ev.filter((e) => e.type === 'content_block_start').map((e) => e.index),
    ).toEqual([0, 1, 2]);
    expect(
      ev.filter((e) => e.type === 'content_block_stop').map((e) => e.index),
    ).toEqual([0, 1, 2]);
    expect(
      ev
        .filter((e) => e.type === 'content_block_delta')
        .map((e) => e.index)
        .filter((i, at, all) => all.indexOf(i) === at),
    ).toEqual([0, 1, 2]);
    // the record is written before the reply is finalized: what is left of the reservation goes
    expect(recording.releaseAssistantRecordUuid).toHaveBeenCalled();
  });

  it('starts again at 0 under the next record for the next reply', () => {
    const { adapter, events } = setup();
    adapter.startAssistantMessage();
    adapter.processEvent({ type: LlmEventType.Content, value: 'one' });
    adapter.finalizeAssistantMessage();
    adapter.startAssistantMessage();
    adapter.processEvent({ type: LlmEventType.Content, value: 'two' });
    adapter.finalizeAssistantMessage();

    const ev = events();
    expect(
      ev.filter((e) => e.type === 'message_start').map((e) => e.message?.id),
    ).toEqual(['record-1', 'record-2']);
    expect(
      ev.filter((e) => e.type === 'content_block_start').map((e) => e.index),
    ).toEqual([0, 0]);
  });
});
