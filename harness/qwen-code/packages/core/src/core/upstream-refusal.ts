/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import type { Content, Part } from '@google/genai';
import { getErrorMessage } from '../utils/errors.js';

/**
 * DE0CH fork: what the chat does when the model provider refuses a request.
 *
 * The model keeps no state: every request is rebuilt from the conversation
 * this harness holds. So data the provider cannot take (a half-written video
 * the agent opened with read_file: "Could not open video stream") is sent
 * again with every later request, and the conversation is dead: each new
 * message fails the same way (YYC AI, 2026-11-16).
 *
 * A refusal therefore rolls the conversation back to the last request the
 * provider answered. What was added since (the delta: this step's tool
 * results, or the person's message) is blamed first, in stages, each tried
 * with one more request:
 *
 *  1. `delta-media`: the videos, sounds, images and files of the delta are
 *     replaced by a line from the harness that names the provider's error;
 *  2. `delta-results`: every tool result of the delta is replaced the same
 *     way (the call keeps its result, so the pairing stays valid);
 *  3. `history-media`: the delta was not it (or had nothing to replace, as
 *     in a conversation that was already dead): the newest earlier entry
 *     that carries media, one entry per request.
 *
 * The replaced delta is what the model then answers: it sees a tool result
 * that failed and decides what to do. A request refused after every stage
 * was not refused for its data: the conversation gets its data back and the
 * error goes to the caller as before.
 *
 * In the transcript the refused entries stay where they were, as a branch
 * nothing reads on resume (ChatRecordingService.supersedeRefusedParts).
 */

/** Statuses that say "this request, as it is, will never be accepted". */
const REFUSAL_STATUSES = new Set([400, 413, 415, 422]);

/** How many earlier entries stage 3 may try, one request each. */
export const MAX_HISTORY_MEDIA_STEPS = 5;

export type RefusalStage = 'delta-media' | 'delta-results' | 'history-media';

/** What one stage replaced; the same plan rewrites the transcript's records. */
export interface RefusalPlan {
  stage: RefusalStage;
  /** Fingerprints of the media parts replaced (mediaFingerprint). */
  media: Set<string>;
  /** Ids of the tool calls whose whole result was replaced. */
  callIds: Set<string>;
  /** The provider's own words. */
  reason: string;
}

/** A stage applied to the conversation in memory. */
export interface AppliedRefusal {
  plan: RefusalPlan;
  /** How many parts were replaced. */
  replaced: number;
  /** Put the conversation back as it was. */
  restore(): void;
}

function statusOf(error: unknown): number | undefined {
  const seen = new Set<unknown>();
  let current: unknown = error;
  while (
    typeof current === 'object' &&
    current !== null &&
    !seen.has(current)
  ) {
    seen.add(current);
    const record = current as Record<string, unknown>;
    for (const key of ['status', 'statusCode', 'code']) {
      const value = record[key];
      if (typeof value === 'number' && value >= 100 && value < 600) {
        return value;
      }
    }
    const nested = record['error'];
    if (typeof nested === 'object' && nested !== null && !seen.has(nested)) {
      const code = (nested as Record<string, unknown>)['code'];
      if (typeof code === 'number' && code >= 100 && code < 600) return code;
    }
    current = record['cause'];
  }
  return undefined;
}

/**
 * The provider's words when it refused the request for what the request is
 * (as an HTTP status or as an error inside an established stream); undefined
 * for everything a later identical request could get past (rate limits,
 * server errors, timeouts, a dropped connection, sign-in).
 */
export function upstreamRefusalReason(error: unknown): string | undefined {
  const status = statusOf(error);
  if (status === undefined || !REFUSAL_STATUSES.has(status)) return undefined;
  const message = getErrorMessage(error).replace(/\s+/g, ' ').trim();
  const text = message.length > 0 ? message : `HTTP ${status}`;
  return text.length > 300 ? `${text.slice(0, 300)}…` : text;
}

/** Names a media part by what it holds, in memory and in the transcript alike. */
export function mediaFingerprint(part: Part): string | undefined {
  if (part.inlineData) {
    const data = part.inlineData.data ?? '';
    return [
      'inline',
      part.inlineData.mimeType ?? '',
      data.length,
      data.slice(0, 64),
      data.slice(-64),
    ].join(':');
  }
  if (part.fileData) {
    return `file:${part.fileData.mimeType ?? ''}:${part.fileData.fileUri ?? ''}`;
  }
  return undefined;
}

function describeMedia(part: Part): string {
  const data = part.inlineData ?? part.fileData;
  const mime = data?.mimeType ?? 'unknown type';
  const kind = mime.startsWith('video/')
    ? 'video'
    : mime.startsWith('audio/')
      ? 'sound clip'
      : mime.startsWith('image/')
        ? 'image'
        : 'file';
  const name = data?.displayName ? ` "${data.displayName}"` : '';
  return `${kind}${name} (${mime})`;
}

export function refusedMediaNotice(part: Part, reason: string): string {
  return (
    `[The harness took this ${describeMedia(part)} out of the conversation: ` +
    `the model provider refused the request that carried it (${reason}). ` +
    `You have not seen or heard it. If it came from a file, check that the ` +
    `file is complete and valid, then open it again.]`
  );
}

export function refusedResultNotice(reason: string): string {
  return (
    `[The harness took this tool result out of the conversation: the model ` +
    `provider refused the request that carried it (${reason}). The tool ran, ` +
    `but you have not seen its output. If you still need it, get it again in ` +
    `a smaller or different form.]`
  );
}

/**
 * The parts with the plan's media and tool results replaced by the harness's
 * notices; undefined when the plan names nothing in them. Never mutates.
 */
export function rewriteRefusedParts(
  parts: readonly Part[] | undefined,
  plan: Pick<RefusalPlan, 'media' | 'callIds' | 'reason'>,
): { parts: Part[]; replaced: number } | undefined {
  if (!parts) return undefined;
  let replaced = 0;
  const out = parts.map((part): Part => {
    const fingerprint = mediaFingerprint(part);
    if (fingerprint && plan.media.has(fingerprint)) {
      replaced++;
      return { text: refusedMediaNotice(part, plan.reason) };
    }
    const response = part.functionResponse;
    if (!response) return part;
    if (response.id && plan.callIds.has(response.id)) {
      replaced++;
      const { parts: _dropped, ...rest } = response;
      return {
        ...part,
        functionResponse: {
          ...rest,
          response: { error: refusedResultNotice(plan.reason) },
        },
      };
    }
    let nestedReplaced = 0;
    const nested = (response.parts ?? []).map((inner) => {
      const innerFingerprint = mediaFingerprint(inner as Part);
      if (innerFingerprint && plan.media.has(innerFingerprint)) {
        nestedReplaced++;
        return { text: refusedMediaNotice(inner as Part, plan.reason) };
      }
      return inner;
    });
    if (nestedReplaced === 0) return part;
    replaced += nestedReplaced;
    return {
      ...part,
      functionResponse: { ...response, parts: nested },
    } as Part;
  });
  return replaced > 0 ? { parts: out, replaced } : undefined;
}

function mediaOf(content: Content): Set<string> {
  const found = new Set<string>();
  for (const part of content.parts ?? []) {
    const own = mediaFingerprint(part);
    if (own) found.add(own);
    for (const inner of part.functionResponse?.parts ?? []) {
      const nested = mediaFingerprint(inner as Part);
      if (nested) found.add(nested);
    }
  }
  return found;
}

function apply(
  content: Content,
  plan: RefusalPlan,
): AppliedRefusal | undefined {
  const rewritten = rewriteRefusedParts(content.parts, plan);
  if (!rewritten) return undefined;
  const original = content.parts;
  content.parts = rewritten.parts;
  return {
    plan,
    replaced: rewritten.replaced,
    restore: () => {
      content.parts = original;
    },
  };
}

/** Stage 1: the delta's media. */
export function replaceDeltaMedia(
  delta: Content,
  reason: string,
): AppliedRefusal | undefined {
  const media = mediaOf(delta);
  if (media.size === 0) return undefined;
  return apply(delta, {
    stage: 'delta-media',
    media,
    callIds: new Set(),
    reason,
  });
}

/** Stage 2: every tool result of the delta. */
export function replaceDeltaResults(
  delta: Content,
  reason: string,
): AppliedRefusal | undefined {
  const callIds = new Set<string>();
  for (const part of delta.parts ?? []) {
    const id = part.functionResponse?.id;
    if (id) callIds.add(id);
  }
  if (callIds.size === 0) return undefined;
  return apply(delta, {
    stage: 'delta-results',
    media: new Set(),
    callIds,
    reason,
  });
}

/**
 * Stage 3: the newest entry before `before` that carries media. `before` is
 * the index of the delta; entries from it on are not looked at.
 */
export function replaceNewestHistoryMedia(
  history: Content[],
  before: number,
  reason: string,
): AppliedRefusal | undefined {
  for (let i = Math.min(before, history.length) - 1; i >= 0; i--) {
    const media = mediaOf(history[i]);
    if (media.size === 0) continue;
    return apply(history[i], {
      stage: 'history-media',
      media,
      callIds: new Set(),
      reason,
    });
  }
  return undefined;
}

/**
 * The stages of one send, in order. `next` applies the following stage to
 * the conversation and returns it, or undefined when nothing is left to
 * replace; `accept` is called when the provider answered a request and
 * returns what to write to the transcript; `giveUp` puts everything back.
 */
export class RefusalRollback {
  private readonly applied: AppliedRefusal[] = [];
  private deltaMediaTried = false;
  private deltaResultsTried = false;
  private historySteps = 0;

  /** `history` is asked each time: the chat may swap its array. */
  constructor(
    private readonly history: () => Content[],
    private readonly delta: Content | undefined,
  ) {}

  /** True once a stage has replaced something in this send. */
  get active(): boolean {
    return this.applied.length > 0;
  }

  next(reason: string): AppliedRefusal | undefined {
    const delta = this.delta;
    let step: AppliedRefusal | undefined;
    if (delta && !this.deltaMediaTried) {
      this.deltaMediaTried = true;
      step = replaceDeltaMedia(delta, reason);
    }
    if (!step && delta && !this.deltaResultsTried) {
      this.deltaResultsTried = true;
      step = replaceDeltaResults(delta, reason);
    }
    if (!step && this.historySteps < MAX_HISTORY_MEDIA_STEPS) {
      this.historySteps++;
      const history = this.history();
      const index = delta ? history.lastIndexOf(delta) : -1;
      step = replaceNewestHistoryMedia(
        history,
        index >= 0 ? index : history.length,
        reason,
      );
    }
    if (step) this.applied.push(step);
    return step;
  }

  /** The provider answered: the replacements stay, and these are their plans. */
  accept(): RefusalPlan[] {
    return this.applied.splice(0).map((step) => step.plan);
  }

  /** Nothing helped: the conversation gets its data back. */
  giveUp(): void {
    for (const step of this.applied.splice(0).reverse()) step.restore();
  }
}
