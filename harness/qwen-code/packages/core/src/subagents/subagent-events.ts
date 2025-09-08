/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { EventEmitter } from 'events';

export type SubAgentEvent =
  | 'start'
  | 'round_start'
  | 'round_end'
  | 'model_text'
  | 'tool_call'
  | 'tool_result'
  | 'finish'
  | 'error';

export interface SubAgentModelTextEvent {
  subagentId: string;
  round: number;
  text: string;
  timestamp: number;
}

export interface SubAgentStartEvent {
  subagentId: string;
  name: string;
  model?: string;
  tools: string[];
  timestamp: number;
}

export interface SubAgentRoundEvent {
  subagentId: string;
  round: number;
  promptId: string;
  timestamp: number;
}

export interface SubAgentToolCallEvent {
  subagentId: string;
  round: number;
  callId: string;
  name: string;
  args: Record<string, unknown>;
  timestamp: number;
}

export interface SubAgentToolResultEvent {
  subagentId: string;
  round: number;
  callId: string;
  name: string;
  success: boolean;
  error?: string;
  durationMs?: number;
  timestamp: number;
}

export interface SubAgentFinishEvent {
  subagentId: string;
  terminate_reason: string;
  timestamp: number;
  rounds?: number;
  totalDurationMs?: number;
  totalToolCalls?: number;
  successfulToolCalls?: number;
  failedToolCalls?: number;
  inputTokens?: number;
  outputTokens?: number;
  totalTokens?: number;
}

export class SubAgentEventEmitter {
  private ee = new EventEmitter();

  on(event: SubAgentEvent, listener: (...args: unknown[]) => void) {
    this.ee.on(event, listener);
  }

  off(event: SubAgentEvent, listener: (...args: unknown[]) => void) {
    this.ee.off(event, listener);
  }

  emit(event: SubAgentEvent, payload: unknown) {
    this.ee.emit(event, payload);
  }
}
