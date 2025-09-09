/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

export interface ToolUsageStats {
  name: string;
  count: number;
  success: number;
  failure: number;
  lastError?: string;
  totalDurationMs: number;
  averageDurationMs: number;
}

export interface SubagentStatsSummary {
  rounds: number;
  totalDurationMs: number;
  totalToolCalls: number;
  successfulToolCalls: number;
  failedToolCalls: number;
  successRate: number;
  inputTokens: number;
  outputTokens: number;
  totalTokens: number;
  estimatedCost: number;
  toolUsage: ToolUsageStats[];
}

export class SubagentStatistics {
  private startTimeMs = 0;
  private rounds = 0;
  private totalToolCalls = 0;
  private successfulToolCalls = 0;
  private failedToolCalls = 0;
  private inputTokens = 0;
  private outputTokens = 0;
  private toolUsage = new Map<string, ToolUsageStats>();

  start(now = Date.now()) {
    this.startTimeMs = now;
  }

  setRounds(rounds: number) {
    this.rounds = rounds;
  }

  recordToolCall(
    name: string,
    success: boolean,
    durationMs: number,
    lastError?: string,
  ) {
    this.totalToolCalls += 1;
    if (success) this.successfulToolCalls += 1;
    else this.failedToolCalls += 1;

    const tu = this.toolUsage.get(name) || {
      name,
      count: 0,
      success: 0,
      failure: 0,
      lastError: undefined,
      totalDurationMs: 0,
      averageDurationMs: 0,
    };
    tu.count += 1;
    if (success) tu.success += 1;
    else tu.failure += 1;
    if (lastError) tu.lastError = lastError;
    tu.totalDurationMs += Math.max(0, durationMs || 0);
    tu.averageDurationMs = tu.count > 0 ? tu.totalDurationMs / tu.count : 0;
    this.toolUsage.set(name, tu);
  }

  recordTokens(input: number, output: number) {
    this.inputTokens += Math.max(0, input || 0);
    this.outputTokens += Math.max(0, output || 0);
  }

  getSummary(now = Date.now()): SubagentStatsSummary {
    const totalDurationMs = this.startTimeMs ? now - this.startTimeMs : 0;
    const totalToolCalls = this.totalToolCalls;
    const successRate =
      totalToolCalls > 0
        ? (this.successfulToolCalls / totalToolCalls) * 100
        : 0;
    const totalTokens = this.inputTokens + this.outputTokens;
    const estimatedCost = this.inputTokens * 3e-5 + this.outputTokens * 6e-5;
    return {
      rounds: this.rounds,
      totalDurationMs,
      totalToolCalls,
      successfulToolCalls: this.successfulToolCalls,
      failedToolCalls: this.failedToolCalls,
      successRate,
      inputTokens: this.inputTokens,
      outputTokens: this.outputTokens,
      totalTokens,
      estimatedCost,
      toolUsage: Array.from(this.toolUsage.values()),
    };
  }
}
