/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * ACP连接类型定义
 *
 * 包含了ACP连接所需的所有类型和接口定义
 */

import type { ChildProcess } from 'child_process';
import type {
  AcpSessionUpdate,
  AcpPermissionRequest,
} from '../shared/acpTypes.js';

/**
 * 待处理的请求信息
 */
export interface PendingRequest<T = unknown> {
  /** 成功回调 */
  resolve: (value: T) => void;
  /** 失败回调 */
  reject: (error: Error) => void;
  /** 超时定时器ID */
  timeoutId?: NodeJS.Timeout;
  /** 请求方法名 */
  method: string;
}

/**
 * ACP连接回调函数类型
 */
export interface AcpConnectionCallbacks {
  /** 会话更新回调 */
  onSessionUpdate: (data: AcpSessionUpdate) => void;
  /** 权限请求回调 */
  onPermissionRequest: (data: AcpPermissionRequest) => Promise<{
    optionId: string;
  }>;
  /** 回合结束回调 */
  onEndTurn: () => void;
}

/**
 * ACP连接状态
 */
export interface AcpConnectionState {
  /** 子进程实例 */
  child: ChildProcess | null;
  /** 待处理的请求映射表 */
  pendingRequests: Map<number, PendingRequest<unknown>>;
  /** 下一个请求ID */
  nextRequestId: number;
  /** 当前会话ID */
  sessionId: string | null;
  /** 是否已初始化 */
  isInitialized: boolean;
  /** 后端类型 */
  backend: string | null;
}
