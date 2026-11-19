/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, expect, it } from 'vitest';
import type OpenAI from 'openai';
import {
  DEFAULT_MAX_REQUEST_AUDIOS,
  DEFAULT_MAX_REQUEST_VIDEOS,
  evictOldestAudioVideoBeyondCaps,
} from './media-budget.js';
import { OpenAIContentConverter } from './converter.js';
import type { RequestContext } from './types.js';

type Message = OpenAI.Chat.ChatCompletionMessageParam;

const video = (name: string) => ({
  type: 'video_url' as const,
  video_url: { url: `data:video/mp4;base64,${name}` },
});
const audio = (name: string) => ({
  type: 'input_audio' as const,
  input_audio: { data: name, format: 'mp3' as const },
});
const img = (url: string) => ({
  type: 'image_url' as const,
  image_url: { url },
});
const txt = (text: string) => ({ type: 'text' as const, text });
const user = (...parts: unknown[]) =>
  ({ role: 'user', content: parts }) as unknown as Message;

function kinds(messages: Message[], type: string): string[] {
  const out: string[] = [];
  for (const m of messages) {
    if (!Array.isArray(m.content)) continue;
    for (const p of m.content as unknown as Array<Record<string, unknown>>) {
      if (p['type'] !== type) continue;
      if (type === 'video_url')
        out.push((p['video_url'] as { url: string }).url.split(',')[1]);
      if (type === 'input_audio')
        out.push((p['input_audio'] as { data: string }).data);
      if (type === 'image_url')
        out.push((p['image_url'] as { url: string }).url);
    }
  }
  return out;
}

describe('evictOldestAudioVideoBeyondCaps', () => {
  it('has the limits of the endpoint this fork runs against', () => {
    // DeepInfra's MiMo endpoint through OpenRouter, measured 2026-11-16:
    // "Too many videos in request: 2 > 1", "At most 2 audio(s) may be
    // provided in one prompt".
    expect(DEFAULT_MAX_REQUEST_VIDEOS).toBe(1);
    expect(DEFAULT_MAX_REQUEST_AUDIOS).toBe(2);
  });

  it('leaves one clip (a video and its sound) untouched', () => {
    const messages = [user(txt('watch'), video('v1'), audio('a1'))];
    const before = JSON.stringify(messages);
    expect(evictOldestAudioVideoBeyondCaps(messages)).toBe(0);
    expect(JSON.stringify(messages)).toBe(before);
  });

  it('keeps the newest video when a second clip is opened', () => {
    const messages = [
      user(txt('first'), video('v1'), audio('a1')),
      { role: 'assistant', content: 'a robot dances' } as Message,
      user(txt('second'), video('v2'), audio('a2')),
    ];
    expect(evictOldestAudioVideoBeyondCaps(messages)).toBe(1);
    expect(kinds(messages, 'video_url')).toEqual(['v2']);
    expect(kinds(messages, 'input_audio')).toEqual(['a1', 'a2']);
  });

  it('keeps the newest two sounds when a third clip is opened', () => {
    const messages = [
      user(video('v1'), audio('a1')),
      user(video('v2'), audio('a2')),
      user(video('v3'), audio('a3')),
    ];
    expect(evictOldestAudioVideoBeyondCaps(messages)).toBe(3);
    expect(kinds(messages, 'video_url')).toEqual(['v3']);
    expect(kinds(messages, 'input_audio')).toEqual(['a2', 'a3']);
  });

  it('says in place of a removed clip that it can be opened again, never that the model cannot take it', () => {
    const messages = [user(video('v1'), audio('a1')), user(video('v2'))];
    evictOldestAudioVideoBeyondCaps(messages, 1, 0);
    const first = messages[0].content as unknown as Array<{
      type: string;
      text?: string;
    }>;
    expect(first.map((p) => p.type)).toEqual(['text', 'text']);
    for (const p of first) {
      expect(p.text).toMatch(/read_file/);
      expect(p.text).not.toMatch(/not support/i);
    }
    expect(first[0].text).toMatch(/video/);
    expect(first[1].text).toMatch(/sound/);
  });

  it('leaves images and text alone', () => {
    const messages = [
      user(txt('a'), img('i1'), video('v1'), img('i2')),
      user(txt('b'), img('i3'), video('v2')),
    ];
    evictOldestAudioVideoBeyondCaps(messages);
    expect(kinds(messages, 'image_url')).toEqual(['i1', 'i2', 'i3']);
    expect(kinds(messages, 'video_url')).toEqual(['v2']);
  });

  it('removes nothing when a cap is negative (no limit)', () => {
    const messages = [user(video('v1')), user(video('v2')), user(video('v3'))];
    expect(evictOldestAudioVideoBeyondCaps(messages, -1, -1)).toBe(0);
    expect(kinds(messages, 'video_url')).toEqual(['v1', 'v2', 'v3']);
  });
});

describe('a request built from a conversation with two clips', () => {
  it('carries one video, so the endpoint takes it', () => {
    const clip = (name: string) => ({
      role: 'user' as const,
      parts: [
        { text: `clip ${name}` },
        { inlineData: { mimeType: 'video/mp4', data: `${name}-video` } },
        { inlineData: { mimeType: 'audio/mpeg', data: `${name}-sound` } },
      ],
    });
    const context = {
      model: 'xiaomi/mimo-v2.6-pro',
      modalities: { image: true, audio: true, video: true },
      startTime: 0,
    } as unknown as RequestContext;
    const messages = OpenAIContentConverter.convertLlmRequestToOpenAI(
      {
        model: 'xiaomi/mimo-v2.6-pro',
        contents: [
          clip('one'),
          { role: 'model', parts: [{ text: 'seen' }] },
          clip('two'),
        ],
      },
      context,
    );
    expect(kinds(messages, 'video_url')).toEqual(['two-video']);
    expect(kinds(messages, 'input_audio')).toEqual(['one-sound', 'two-sound']);
  });
});
