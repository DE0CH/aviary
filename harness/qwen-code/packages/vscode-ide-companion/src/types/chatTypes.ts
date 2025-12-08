/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */
import type { AcpPermissionRequest, ApprovalModeValue } from './acpTypes.js';

export interface ChatMessage {
  role: 'user' | 'assistant';
  content: string;
  timestamp: number;
}

/**
 * Plan Entry
 * Represents a single step in the AI's execution plan
 */
export interface PlanEntry {
  /** The detailed description of this plan step */
  content: string;
  /** The priority level of this plan step */
  priority?: 'high' | 'medium' | 'low';
  /** The current execution status of this plan step */
  status: 'pending' | 'in_progress' | 'completed';
}

/**
 * Tool Call Update Data
 * Contains information about a tool call execution or update
 */
export interface ToolCallUpdateData {
  /** Unique identifier for this tool call */
  toolCallId: string;
  /** The type of tool being called (e.g., 'read', 'write', 'execute') */
  kind?: string;
  /** Human-readable title or description of the tool call */
  title?: string;
  /** Current execution status of the tool call */
  status?: string;
  /** Raw input parameters passed to the tool */
  rawInput?: unknown;
  /** Content or output data from the tool execution */
  content?: Array<Record<string, unknown>>;
  /** File locations associated with this tool call */
  locations?: Array<{ path: string; line?: number | null }>;
}

/**
 * Callback Functions Collection
 * Defines all possible callback functions for the Qwen Agent
 */
export interface QwenAgentCallbacks {
  /** Callback for receiving chat messages from the agent */
  onMessage?: (message: ChatMessage) => void;
  /** Callback for receiving streamed text chunks during generation */
  onStreamChunk?: (chunk: string) => void;
  /** Callback for receiving thought process chunks during generation */
  onThoughtChunk?: (chunk: string) => void;
  /** Callback for receiving tool call updates during execution */
  onToolCall?: (update: ToolCallUpdateData) => void;
  /** Callback for receiving execution plan updates */
  onPlan?: (entries: PlanEntry[]) => void;
  /** Callback for handling permission requests from the agent */
  onPermissionRequest?: (request: AcpPermissionRequest) => Promise<string>;
  /** Callback triggered when the agent reaches the end of a turn */
  onEndTurn?: () => void;
  /** Callback for receiving mode information after ACP initialization */
  onModeInfo?: (info: {
    currentModeId?: ApprovalModeValue;
    availableModes?: Array<{
      id: ApprovalModeValue;
      name: string;
      description: string;
    }>;
  }) => void;
  /** Callback for receiving notifications when the mode changes */
  onModeChanged?: (modeId: ApprovalModeValue) => void;
}

/**
 * Tool call update type
 */
export interface ToolCallUpdate {
  type: 'tool_call' | 'tool_call_update';
  toolCallId: string;
  kind?: string;
  title?: string;
  status?: 'pending' | 'in_progress' | 'completed' | 'failed';
  rawInput?: unknown;
  content?: Array<{
    type: 'content' | 'diff';
    content?: {
      type: string;
      text?: string;
      [key: string]: unknown;
    };
    path?: string;
    oldText?: string | null;
    newText?: string;
    [key: string]: unknown;
  }>;
  locations?: Array<{
    path: string;
    line?: number | null;
  }>;
  timestamp?: number; // Add timestamp field for message ordering
}
