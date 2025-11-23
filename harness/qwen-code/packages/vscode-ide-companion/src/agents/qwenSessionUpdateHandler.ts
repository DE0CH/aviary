/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * Qwen会话更新处理器
 *
 * 负责处理来自ACP的会话更新，并分发到相应的回调函数
 */

import type { AcpSessionUpdate } from '../shared/acpTypes.js';
import type { QwenAgentCallbacks } from './qwenTypes.js';

/**
 * Qwen会话更新处理器类
 * 处理各种会话更新事件并调用相应的回调
 */
export class QwenSessionUpdateHandler {
  private callbacks: QwenAgentCallbacks;

  constructor(callbacks: QwenAgentCallbacks) {
    this.callbacks = callbacks;
  }

  /**
   * 更新回调函数
   *
   * @param callbacks - 新的回调函数集合
   */
  updateCallbacks(callbacks: QwenAgentCallbacks): void {
    this.callbacks = callbacks;
  }

  /**
   * 处理会话更新
   *
   * @param data - ACP会话更新数据
   */
  handleSessionUpdate(data: AcpSessionUpdate): void {
    const update = data.update;
    console.log(
      '[SessionUpdateHandler] Processing update type:',
      update.sessionUpdate,
    );

    switch (update.sessionUpdate) {
      case 'user_message_chunk':
        // 处理用户消息块
        if (update.content?.text && this.callbacks.onStreamChunk) {
          this.callbacks.onStreamChunk(update.content.text);
        }
        break;

      case 'agent_message_chunk':
        // 处理助手消息块
        if (update.content?.text && this.callbacks.onStreamChunk) {
          this.callbacks.onStreamChunk(update.content.text);
        }
        break;

      case 'agent_thought_chunk':
        // 处理思考块 - 使用特殊回调
        console.log(
          '[SessionUpdateHandler] 🧠 THOUGHT CHUNK:',
          update.content?.text,
        );
        if (update.content?.text) {
          if (this.callbacks.onThoughtChunk) {
            console.log(
              '[SessionUpdateHandler] 🧠 Calling onThoughtChunk callback',
            );
            this.callbacks.onThoughtChunk(update.content.text);
          } else if (this.callbacks.onStreamChunk) {
            // 回退到常规流处理
            console.log(
              '[SessionUpdateHandler] 🧠 Falling back to onStreamChunk',
            );
            this.callbacks.onStreamChunk(update.content.text);
          }
        }
        break;

      case 'tool_call': {
        // 处理新的工具调用
        if (this.callbacks.onToolCall && 'toolCallId' in update) {
          this.callbacks.onToolCall({
            toolCallId: update.toolCallId as string,
            kind: (update.kind as string) || undefined,
            title: (update.title as string) || undefined,
            status: (update.status as string) || undefined,
            rawInput: update.rawInput,
            content: update.content as
              | Array<Record<string, unknown>>
              | undefined,
            locations: update.locations as
              | Array<{ path: string; line?: number | null }>
              | undefined,
          });
        }
        break;
      }

      case 'tool_call_update': {
        // 处理工具调用状态更新
        if (this.callbacks.onToolCall && 'toolCallId' in update) {
          this.callbacks.onToolCall({
            toolCallId: update.toolCallId as string,
            kind: (update.kind as string) || undefined,
            title: (update.title as string) || undefined,
            status: (update.status as string) || undefined,
            rawInput: update.rawInput,
            content: update.content as
              | Array<Record<string, unknown>>
              | undefined,
            locations: update.locations as
              | Array<{ path: string; line?: number | null }>
              | undefined,
          });
        }
        break;
      }

      case 'plan': {
        // 处理计划更新
        if ('entries' in update) {
          const entries = update.entries as Array<{
            content: string;
            priority: 'high' | 'medium' | 'low';
            status: 'pending' | 'in_progress' | 'completed';
          }>;

          if (this.callbacks.onPlan) {
            this.callbacks.onPlan(entries);
          } else if (this.callbacks.onStreamChunk) {
            // 回退到流处理
            const planText =
              '\n📋 Plan:\n' +
              entries
                .map(
                  (entry, i) =>
                    `${i + 1}. [${entry.priority}] ${entry.content}`,
                )
                .join('\n');
            this.callbacks.onStreamChunk(planText);
          }
        }
        break;
      }

      default:
        console.log('[QwenAgentManager] Unhandled session update type');
        break;
    }
  }
}
