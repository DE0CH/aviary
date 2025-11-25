/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * Qwen Agent Manager Type Definitions
 *
 * Contains all related interfaces and type definitions
 */

import type { AcpPermissionRequest } from '../shared/acpTypes.js';

/**
 * Chat Message
 */
export interface ChatMessage {
  /** Message role: user or assistant */
  role: 'user' | 'assistant';
  /** Message content */
  content: string;
  /** Timestamp */
  timestamp: number;
}

/**
 * Plan Entry
 */
export interface PlanEntry {
  /** Entry content */
  content: string;
  /** Priority */
  priority: 'high' | 'medium' | 'low';
  /** Status */
  status: 'pending' | 'in_progress' | 'completed';
}

/**
 * Tool Call Update Data
 */
export interface ToolCallUpdateData {
  /** Tool call ID */
  toolCallId: string;
  /** Tool type */
  kind?: string;
  /** Tool title */
  title?: string;
  /** Status */
  status?: string;
  /** Raw input */
  rawInput?: unknown;
  /** Content */
  content?: Array<Record<string, unknown>>;
  /** Location information */
  locations?: Array<{ path: string; line?: number | null }>;
}

/**
 * Callback Functions Collection
 */
export interface QwenAgentCallbacks {
  /** Message callback */
  onMessage?: (message: ChatMessage) => void;
  /** Stream text chunk callback */
  onStreamChunk?: (chunk: string) => void;
  /** Thought text chunk callback */
  onThoughtChunk?: (chunk: string) => void;
  /** Tool call callback */
  onToolCall?: (update: ToolCallUpdateData) => void;
  /** Plan callback */
  onPlan?: (entries: PlanEntry[]) => void;
  /** Permission request callback */
  onPermissionRequest?: (request: AcpPermissionRequest) => Promise<string>;
}
