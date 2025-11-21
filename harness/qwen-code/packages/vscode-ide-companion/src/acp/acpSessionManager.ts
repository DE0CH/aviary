/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * ACP会话管理器
 *
 * 负责管理ACP协议的会话操作，包括初始化、认证、会话创建和切换等
 */

import { JSONRPC_VERSION } from '../shared/acpTypes.js';
import type {
  AcpRequest,
  AcpNotification,
  AcpResponse,
} from '../shared/acpTypes.js';
import { AGENT_METHODS, CUSTOM_METHODS } from './schema.js';
import type { PendingRequest } from './connectionTypes.js';
import type { ChildProcess } from 'child_process';

/**
 * ACP会话管理器类
 * 提供会话的初始化、认证、创建、加载和切换功能
 */
export class AcpSessionManager {
  private sessionId: string | null = null;
  private isInitialized = false;

  /**
   * 发送请求到ACP服务器
   *
   * @param method - 请求方法名
   * @param params - 请求参数
   * @param child - 子进程实例
   * @param pendingRequests - 待处理请求映射表
   * @param nextRequestId - 请求ID计数器
   * @returns 请求响应
   */
  private sendRequest<T = unknown>(
    method: string,
    params: Record<string, unknown> | undefined,
    child: ChildProcess | null,
    pendingRequests: Map<number, PendingRequest<unknown>>,
    nextRequestId: { value: number },
  ): Promise<T> {
    const id = nextRequestId.value++;
    const message: AcpRequest = {
      jsonrpc: JSONRPC_VERSION,
      id,
      method,
      ...(params && { params }),
    };

    return new Promise((resolve, reject) => {
      const timeoutDuration =
        method === AGENT_METHODS.session_prompt ? 120000 : 60000;

      const timeoutId = setTimeout(() => {
        pendingRequests.delete(id);
        reject(new Error(`Request ${method} timed out`));
      }, timeoutDuration);

      const pendingRequest: PendingRequest<T> = {
        resolve: (value: T) => {
          clearTimeout(timeoutId);
          resolve(value);
        },
        reject: (error: Error) => {
          clearTimeout(timeoutId);
          reject(error);
        },
        timeoutId,
        method,
      };

      pendingRequests.set(id, pendingRequest as PendingRequest<unknown>);
      this.sendMessage(message, child);
    });
  }

  /**
   * 发送消息到子进程
   *
   * @param message - 请求或通知消息
   * @param child - 子进程实例
   */
  private sendMessage(
    message: AcpRequest | AcpNotification,
    child: ChildProcess | null,
  ): void {
    if (child?.stdin) {
      const jsonString = JSON.stringify(message);
      const lineEnding = process.platform === 'win32' ? '\r\n' : '\n';
      child.stdin.write(jsonString + lineEnding);
    }
  }

  /**
   * 初始化ACP协议连接
   *
   * @param child - 子进程实例
   * @param pendingRequests - 待处理请求映射表
   * @param nextRequestId - 请求ID计数器
   * @returns 初始化响应
   */
  async initialize(
    child: ChildProcess | null,
    pendingRequests: Map<number, PendingRequest<unknown>>,
    nextRequestId: { value: number },
  ): Promise<AcpResponse> {
    const initializeParams = {
      protocolVersion: 1,
      clientCapabilities: {
        fs: {
          readTextFile: true,
          writeTextFile: true,
        },
      },
    };

    console.log('[ACP] Sending initialize request...');
    const response = await this.sendRequest<AcpResponse>(
      AGENT_METHODS.initialize,
      initializeParams,
      child,
      pendingRequests,
      nextRequestId,
    );
    this.isInitialized = true;

    console.log('[ACP] Initialize successful');
    return response;
  }

  /**
   * 进行认证
   *
   * @param methodId - 认证方法ID
   * @param child - 子进程实例
   * @param pendingRequests - 待处理请求映射表
   * @param nextRequestId - 请求ID计数器
   * @returns 认证响应
   */
  async authenticate(
    methodId: string | undefined,
    child: ChildProcess | null,
    pendingRequests: Map<number, PendingRequest<unknown>>,
    nextRequestId: { value: number },
  ): Promise<AcpResponse> {
    const authMethodId = methodId || 'default';
    console.log(
      '[ACP] Sending authenticate request with methodId:',
      authMethodId,
    );
    const response = await this.sendRequest<AcpResponse>(
      AGENT_METHODS.authenticate,
      {
        methodId: authMethodId,
      },
      child,
      pendingRequests,
      nextRequestId,
    );
    console.log('[ACP] Authenticate successful');
    return response;
  }

  /**
   * 创建新会话
   *
   * @param cwd - 工作目录
   * @param child - 子进程实例
   * @param pendingRequests - 待处理请求映射表
   * @param nextRequestId - 请求ID计数器
   * @returns 新会话响应
   */
  async newSession(
    cwd: string,
    child: ChildProcess | null,
    pendingRequests: Map<number, PendingRequest<unknown>>,
    nextRequestId: { value: number },
  ): Promise<AcpResponse> {
    console.log('[ACP] Sending session/new request with cwd:', cwd);
    const response = await this.sendRequest<
      AcpResponse & { sessionId?: string }
    >(
      AGENT_METHODS.session_new,
      {
        cwd,
        mcpServers: [],
      },
      child,
      pendingRequests,
      nextRequestId,
    );

    this.sessionId = response.sessionId || null;
    console.log('[ACP] Session created with ID:', this.sessionId);
    return response;
  }

  /**
   * 发送提示消息
   *
   * @param prompt - 提示内容
   * @param child - 子进程实例
   * @param pendingRequests - 待处理请求映射表
   * @param nextRequestId - 请求ID计数器
   * @returns 响应
   * @throws 当没有活动会话时抛出错误
   */
  async sendPrompt(
    prompt: string,
    child: ChildProcess | null,
    pendingRequests: Map<number, PendingRequest<unknown>>,
    nextRequestId: { value: number },
  ): Promise<AcpResponse> {
    if (!this.sessionId) {
      throw new Error('No active ACP session');
    }

    return await this.sendRequest(
      AGENT_METHODS.session_prompt,
      {
        sessionId: this.sessionId,
        prompt: [{ type: 'text', text: prompt }],
      },
      child,
      pendingRequests,
      nextRequestId,
    );
  }

  /**
   * 加载已有会话
   *
   * @param sessionId - 会话ID
   * @param child - 子进程实例
   * @param pendingRequests - 待处理请求映射表
   * @param nextRequestId - 请求ID计数器
   * @returns 加载响应
   */
  async loadSession(
    sessionId: string,
    child: ChildProcess | null,
    pendingRequests: Map<number, PendingRequest<unknown>>,
    nextRequestId: { value: number },
  ): Promise<AcpResponse> {
    console.log('[ACP] Loading session:', sessionId);
    const response = await this.sendRequest<AcpResponse>(
      AGENT_METHODS.session_load,
      {
        sessionId,
        cwd: process.cwd(),
        mcpServers: [],
      },
      child,
      pendingRequests,
      nextRequestId,
    );
    console.log('[ACP] Session load response:', response);
    return response;
  }

  /**
   * 获取会话列表
   *
   * @param child - 子进程实例
   * @param pendingRequests - 待处理请求映射表
   * @param nextRequestId - 请求ID计数器
   * @returns 会话列表响应
   */
  async listSessions(
    child: ChildProcess | null,
    pendingRequests: Map<number, PendingRequest<unknown>>,
    nextRequestId: { value: number },
  ): Promise<AcpResponse> {
    console.log('[ACP] Requesting session list...');
    try {
      const response = await this.sendRequest<AcpResponse>(
        CUSTOM_METHODS.session_list,
        {},
        child,
        pendingRequests,
        nextRequestId,
      );
      console.log(
        '[ACP] Session list response:',
        JSON.stringify(response).substring(0, 200),
      );
      return response;
    } catch (error) {
      console.error('[ACP] Failed to get session list:', error);
      throw error;
    }
  }

  /**
   * 切换到指定会话
   *
   * @param sessionId - 会话ID
   * @param nextRequestId - 请求ID计数器
   * @returns 切换响应
   */
  async switchSession(
    sessionId: string,
    nextRequestId: { value: number },
  ): Promise<AcpResponse> {
    console.log('[ACP] Switching to session:', sessionId);
    this.sessionId = sessionId;

    const mockResponse: AcpResponse = {
      jsonrpc: JSONRPC_VERSION,
      id: nextRequestId.value++,
      result: { sessionId },
    };
    console.log(
      '[ACP] Session ID updated locally (switch not supported by CLI)',
    );
    return mockResponse;
  }

  /**
   * 取消当前会话的提示生成
   *
   * @param child - 子进程实例
   */
  async cancelSession(child: ChildProcess | null): Promise<void> {
    if (!this.sessionId) {
      console.warn('[ACP] No active session to cancel');
      return;
    }

    console.log('[ACP] Cancelling session:', this.sessionId);

    const cancelParams = {
      sessionId: this.sessionId,
    };

    const message: AcpNotification = {
      jsonrpc: JSONRPC_VERSION,
      method: AGENT_METHODS.session_cancel,
      params: cancelParams,
    };

    this.sendMessage(message, child);
    console.log('[ACP] Cancel notification sent');
  }

  /**
   * 保存当前会话
   *
   * @param tag - 保存标签
   * @param child - 子进程实例
   * @param pendingRequests - 待处理请求映射表
   * @param nextRequestId - 请求ID计数器
   * @returns 保存响应
   */
  async saveSession(
    tag: string,
    child: ChildProcess | null,
    pendingRequests: Map<number, PendingRequest<unknown>>,
    nextRequestId: { value: number },
  ): Promise<AcpResponse> {
    if (!this.sessionId) {
      throw new Error('No active ACP session');
    }

    console.log('[ACP] Saving session with tag:', tag);
    const response = await this.sendRequest<AcpResponse>(
      AGENT_METHODS.session_save,
      {
        sessionId: this.sessionId,
        tag,
      },
      child,
      pendingRequests,
      nextRequestId,
    );
    console.log('[ACP] Session save response:', response);
    return response;
  }

  /**
   * 重置会话管理器状态
   */
  reset(): void {
    this.sessionId = null;
    this.isInitialized = false;
  }

  /**
   * 获取当前会话ID
   */
  getCurrentSessionId(): string | null {
    return this.sessionId;
  }

  /**
   * 检查是否已初始化
   */
  getIsInitialized(): boolean {
    return this.isInitialized;
  }
}
