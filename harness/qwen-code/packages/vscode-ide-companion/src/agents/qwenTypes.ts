/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * Qwen Agent Manager 类型定义
 *
 * 包含所有相关的接口和类型定义
 */

import type { AcpPermissionRequest } from '../shared/acpTypes.js';

/**
 * 聊天消息
 */
export interface ChatMessage {
  /** 消息角色：用户或助手 */
  role: 'user' | 'assistant';
  /** 消息内容 */
  content: string;
  /** 时间戳 */
  timestamp: number;
}

/**
 * 计划条目
 */
export interface PlanEntry {
  /** 条目内容 */
  content: string;
  /** 优先级 */
  priority: 'high' | 'medium' | 'low';
  /** 状态 */
  status: 'pending' | 'in_progress' | 'completed';
}

/**
 * 工具调用更新数据
 */
export interface ToolCallUpdateData {
  /** 工具调用ID */
  toolCallId: string;
  /** 工具类型 */
  kind?: string;
  /** 工具标题 */
  title?: string;
  /** 状态 */
  status?: string;
  /** 原始输入 */
  rawInput?: unknown;
  /** 内容 */
  content?: Array<Record<string, unknown>>;
  /** 位置信息 */
  locations?: Array<{ path: string; line?: number | null }>;
}

/**
 * 回调函数集合
 */
export interface QwenAgentCallbacks {
  /** 消息回调 */
  onMessage?: (message: ChatMessage) => void;
  /** 流式文本块回调 */
  onStreamChunk?: (chunk: string) => void;
  /** 思考文本块回调 */
  onThoughtChunk?: (chunk: string) => void;
  /** 工具调用回调 */
  onToolCall?: (update: ToolCallUpdateData) => void;
  /** 计划回调 */
  onPlan?: (entries: PlanEntry[]) => void;
  /** 权限请求回调 */
  onPermissionRequest?: (request: AcpPermissionRequest) => Promise<string>;
}
