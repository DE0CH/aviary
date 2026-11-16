/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import type OpenAI from 'openai';

/**
 * DE0CH fork: caps on the video and audio parts of one assembled request.
 *
 * A clip the agent opens with read_file stays in the conversation and is sent
 * again with every later request. The endpoint this fork runs against
 * (DeepInfra's MiMo through OpenRouter) refuses a request with more than one
 * video ("Too many videos in request: 2 > 1") or more than two audio parts
 * ("At most 2 audio(s) may be provided in one prompt"), measured 2026-11-16.
 * A refused request made the pipeline retry with ALL media replaced by
 * "[Unsupported video file …]", so from the second clip on the model was
 * told, on every request, that it takes no video, sound or images.
 *
 * So only the newest clips ride along; an older one is replaced where it was
 * by a line that says how to see it again.
 */
export const DEFAULT_MAX_REQUEST_VIDEOS = 1;
export const DEFAULT_MAX_REQUEST_AUDIOS = 2;

const EVICTED_VIDEO_PLACEHOLDER =
  '[An earlier video was here. It was taken out of view because a newer clip was opened and only one video fits in a request. To watch it again, open the file again with read_file.]';
const EVICTED_AUDIO_PLACEHOLDER =
  '[An earlier sound clip was here. It was taken out of view because newer clips were opened and only two sound clips fit in a request. To hear it again, open the file again with read_file.]';

type Message = OpenAI.Chat.ChatCompletionMessageParam;

function isPartOfType(part: unknown, type: string): boolean {
  return (
    typeof part === 'object' &&
    part !== null &&
    (part as { type?: unknown }).type === type
  );
}

/** Replace all but the newest `cap` parts of `type` with `placeholder`. */
function evictOldest(
  messages: Message[],
  type: string,
  cap: number,
  placeholder: string,
): number {
  if (cap < 0) return 0;
  let total = 0;
  for (const message of messages) {
    if (!Array.isArray(message.content)) continue;
    for (const part of message.content) {
      if (isPartOfType(part, type)) total++;
    }
  }
  let toEvict = total - cap;
  if (toEvict <= 0) return 0;

  let evicted = 0;
  // History is chronological, so the oldest parts come first.
  for (const message of messages) {
    if (toEvict === 0) break;
    if (!Array.isArray(message.content)) continue;
    const content = message.content as unknown as Array<
      Record<string, unknown>
    >;
    for (let i = 0; i < content.length && toEvict > 0; i++) {
      if (isPartOfType(content[i], type)) {
        content[i] = { type: 'text', text: placeholder };
        toEvict--;
        evicted++;
      }
    }
  }
  return evicted;
}

/**
 * Keep only the newest `maxVideos` video parts and the newest `maxAudios`
 * audio parts across the whole request. A negative cap means no limit.
 * Images are not touched (see image-budget.ts).
 *
 * Mutates `messages` in place and returns the number of parts replaced.
 */
export function evictOldestAudioVideoBeyondCaps(
  messages: Message[],
  maxVideos: number = DEFAULT_MAX_REQUEST_VIDEOS,
  maxAudios: number = DEFAULT_MAX_REQUEST_AUDIOS,
): number {
  return (
    evictOldest(messages, 'video_url', maxVideos, EVICTED_VIDEO_PLACEHOLDER) +
    evictOldest(messages, 'input_audio', maxAudios, EVICTED_AUDIO_PLACEHOLDER)
  );
}
