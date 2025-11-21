/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import { AcpConnection } from '../acp/acpConnection.js';
import type {
  AcpSessionUpdate,
  AcpPermissionRequest,
} from '../shared/acpTypes.js';
import {
  QwenSessionReader,
  type QwenSession,
} from '../services/qwenSessionReader.js';
import { QwenSessionManager } from '../services/qwenSessionManager.js';
import type { AuthStateManager } from '../auth/authStateManager.js';
import type {
  ChatMessage,
  PlanEntry,
  ToolCallUpdateData,
  QwenAgentCallbacks,
} from './qwenTypes.js';
import { QwenConnectionHandler } from './qwenConnectionHandler.js';
import { QwenSessionUpdateHandler } from './qwenSessionUpdateHandler.js';
import * as crypto from 'crypto';

export type { ChatMessage, PlanEntry, ToolCallUpdateData };

/**
 * Qwen Agent管理器
 *
 * 协调各个模块，提供统一的接口
 */
export class QwenAgentManager {
  private connection: AcpConnection;
  private sessionReader: QwenSessionReader;
  private sessionManager: QwenSessionManager;
  private connectionHandler: QwenConnectionHandler;
  private sessionUpdateHandler: QwenSessionUpdateHandler;
  private currentWorkingDir: string = process.cwd();

  // 回调函数存储
  private callbacks: QwenAgentCallbacks = {};

  constructor() {
    this.connection = new AcpConnection();
    this.sessionReader = new QwenSessionReader();
    this.sessionManager = new QwenSessionManager();
    this.connectionHandler = new QwenConnectionHandler();
    this.sessionUpdateHandler = new QwenSessionUpdateHandler({});

    // 设置ACP连接的回调
    this.connection.onSessionUpdate = (data: AcpSessionUpdate) => {
      this.sessionUpdateHandler.handleSessionUpdate(data);
    };

    this.connection.onPermissionRequest = async (
      data: AcpPermissionRequest,
    ) => {
      if (this.callbacks.onPermissionRequest) {
        const optionId = await this.callbacks.onPermissionRequest(data);
        return { optionId };
      }
      return { optionId: 'allow_once' };
    };

    this.connection.onEndTurn = () => {
      // 通知UI响应完成
    };
  }

  /**
   * 连接到Qwen服务
   *
   * @param workingDir - 工作目录
   * @param authStateManager - 认证状态管理器（可选）
   */
  async connect(
    workingDir: string,
    authStateManager?: AuthStateManager,
  ): Promise<void> {
    this.currentWorkingDir = workingDir;
    await this.connectionHandler.connect(
      this.connection,
      this.sessionReader,
      workingDir,
      authStateManager,
    );
  }

  /**
   * 发送消息
   *
   * @param message - 消息内容
   */
  async sendMessage(message: string): Promise<void> {
    await this.connection.sendPrompt(message);
  }

  /**
   * 获取会话列表
   *
   * @returns 会话列表
   */
  async getSessionList(): Promise<Array<Record<string, unknown>>> {
    try {
      const sessions = await this.sessionReader.getAllSessions(undefined, true);
      console.log(
        '[QwenAgentManager] Session list from files (all projects):',
        sessions.length,
      );

      return sessions.map(
        (session: QwenSession): Record<string, unknown> => ({
          id: session.sessionId,
          sessionId: session.sessionId,
          title: this.sessionReader.getSessionTitle(session),
          name: this.sessionReader.getSessionTitle(session),
          startTime: session.startTime,
          lastUpdated: session.lastUpdated,
          messageCount: session.messages.length,
          projectHash: session.projectHash,
        }),
      );
    } catch (error) {
      console.error('[QwenAgentManager] Failed to get session list:', error);
      return [];
    }
  }

  /**
   * 获取会话消息（从磁盘读取）
   *
   * @param sessionId - 会话ID
   * @returns 消息列表
   */
  async getSessionMessages(sessionId: string): Promise<ChatMessage[]> {
    try {
      const session = await this.sessionReader.getSession(
        sessionId,
        this.currentWorkingDir,
      );
      if (!session) {
        return [];
      }

      return session.messages.map(
        (msg: { type: string; content: string; timestamp: string }) => ({
          role:
            msg.type === 'user' ? ('user' as const) : ('assistant' as const),
          content: msg.content,
          timestamp: new Date(msg.timestamp).getTime(),
        }),
      );
    } catch (error) {
      console.error(
        '[QwenAgentManager] Failed to get session messages:',
        error,
      );
      return [];
    }
  }

  /**
   * 通过 ACP session/save 方法保存会话
   *
   * @param sessionId - 会话ID
   * @param tag - 保存标签
   * @returns 保存响应
   */
  async saveSessionViaAcp(
    sessionId: string,
    tag: string,
  ): Promise<{ success: boolean; message?: string }> {
    try {
      console.log(
        '[QwenAgentManager] Saving session via ACP:',
        sessionId,
        'with tag:',
        tag,
      );
      const response = await this.connection.saveSession(tag);
      console.log('[QwenAgentManager] Session save response:', response);
      // Extract message from response result or error
      let message = '';
      if (response?.result) {
        if (typeof response.result === 'string') {
          message = response.result;
        } else if (
          typeof response.result === 'object' &&
          response.result !== null
        ) {
          // Try to get message from result object
          message =
            (response.result as { message?: string }).message ||
            JSON.stringify(response.result);
        } else {
          message = String(response.result);
        }
      } else if (response?.error) {
        message = response.error.message;
      }

      return { success: true, message };
    } catch (error) {
      console.error('[QwenAgentManager] Session save via ACP failed:', error);
      return {
        success: false,
        message: error instanceof Error ? error.message : String(error),
      };
    }
  }

  /**
   * 直接保存会话到文件系统（不依赖 ACP）
   *
   * @param messages - 当前会话消息
   * @param sessionName - 会话名称
   * @returns 保存结果
   */
  async saveSessionDirect(
    messages: ChatMessage[],
    sessionName: string,
  ): Promise<{ success: boolean; sessionId?: string; message?: string }> {
    try {
      console.log('[QwenAgentManager] Saving session directly:', sessionName);

      // 转换消息格式
      const qwenMessages = messages.map((msg) => ({
        id: crypto.randomUUID(),
        timestamp: new Date(msg.timestamp).toISOString(),
        type: msg.role === 'user' ? ('user' as const) : ('qwen' as const),
        content: msg.content,
      }));

      // 保存会话
      const sessionId = await this.sessionManager.saveSession(
        qwenMessages,
        sessionName,
        this.currentWorkingDir,
      );

      console.log('[QwenAgentManager] Session saved directly:', sessionId);
      return {
        success: true,
        sessionId,
        message: `会话已保存: ${sessionName}`,
      };
    } catch (error) {
      console.error('[QwenAgentManager] Session save directly failed:', error);
      return {
        success: false,
        message: error instanceof Error ? error.message : String(error),
      };
    }
  }

  /**
   * 尝试通过 ACP session/load 方法加载会话
   * 这是一个测试方法，用于验证 CLI 是否支持 session/load
   *
   * @param sessionId - 会话ID
   * @returns 加载响应或错误
   */
  async loadSessionViaAcp(sessionId: string): Promise<unknown> {
    try {
      console.log(
        '[QwenAgentManager] Testing session/load via ACP for:',
        sessionId,
      );
      const response = await this.connection.loadSession(sessionId);
      console.log('[QwenAgentManager] Session load response:', response);
      return response;
    } catch (error) {
      console.error('[QwenAgentManager] Session load via ACP failed:', error);
      throw error;
    }
  }

  /**
   * 直接从文件系统加载会话（不依赖 ACP）
   *
   * @param sessionId - 会话ID
   * @returns 加载的会话消息或null
   */
  async loadSessionDirect(sessionId: string): Promise<ChatMessage[] | null> {
    try {
      console.log('[QwenAgentManager] Loading session directly:', sessionId);

      // 加载会话
      const session = await this.sessionManager.loadSession(
        sessionId,
        this.currentWorkingDir,
      );

      if (!session) {
        console.log('[QwenAgentManager] Session not found:', sessionId);
        return null;
      }

      // 转换消息格式
      const messages: ChatMessage[] = session.messages.map((msg) => ({
        role: msg.type === 'user' ? 'user' : 'assistant',
        content: msg.content,
        timestamp: new Date(msg.timestamp).getTime(),
      }));

      console.log('[QwenAgentManager] Session loaded directly:', sessionId);
      return messages;
    } catch (error) {
      console.error('[QwenAgentManager] Session load directly failed:', error);
      return null;
    }
  }

  /**
   * 创建新会话
   *
   * @param workingDir - 工作目录
   * @returns 新创建的 session ID
   */
  async createNewSession(workingDir: string): Promise<string | null> {
    console.log('[QwenAgentManager] Creating new session...');
    await this.connection.newSession(workingDir);
    const newSessionId = this.connection.currentSessionId;
    console.log(
      '[QwenAgentManager] New session created with ID:',
      newSessionId,
    );
    return newSessionId;
  }

  /**
   * 切换到指定会话
   *
   * @param sessionId - 会话ID
   */
  async switchToSession(sessionId: string): Promise<void> {
    await this.connection.switchSession(sessionId);
  }

  /**
   * 取消当前提示
   */
  async cancelCurrentPrompt(): Promise<void> {
    console.log('[QwenAgentManager] Cancelling current prompt');
    await this.connection.cancelSession();
  }

  /**
   * 注册消息回调
   *
   * @param callback - 消息回调函数
   */
  onMessage(callback: (message: ChatMessage) => void): void {
    this.callbacks.onMessage = callback;
    this.sessionUpdateHandler.updateCallbacks(this.callbacks);
  }

  /**
   * 注册流式文本块回调
   *
   * @param callback - 流式文本块回调函数
   */
  onStreamChunk(callback: (chunk: string) => void): void {
    this.callbacks.onStreamChunk = callback;
    this.sessionUpdateHandler.updateCallbacks(this.callbacks);
  }

  /**
   * 注册思考文本块回调
   *
   * @param callback - 思考文本块回调函数
   */
  onThoughtChunk(callback: (chunk: string) => void): void {
    this.callbacks.onThoughtChunk = callback;
    this.sessionUpdateHandler.updateCallbacks(this.callbacks);
  }

  /**
   * 注册工具调用回调
   *
   * @param callback - 工具调用回调函数
   */
  onToolCall(callback: (update: ToolCallUpdateData) => void): void {
    this.callbacks.onToolCall = callback;
    this.sessionUpdateHandler.updateCallbacks(this.callbacks);
  }

  /**
   * 注册计划回调
   *
   * @param callback - 计划回调函数
   */
  onPlan(callback: (entries: PlanEntry[]) => void): void {
    this.callbacks.onPlan = callback;
    this.sessionUpdateHandler.updateCallbacks(this.callbacks);
  }

  /**
   * 注册权限请求回调
   *
   * @param callback - 权限请求回调函数
   */
  onPermissionRequest(
    callback: (request: AcpPermissionRequest) => Promise<string>,
  ): void {
    this.callbacks.onPermissionRequest = callback;
    this.sessionUpdateHandler.updateCallbacks(this.callbacks);
  }

  /**
   * 断开连接
   */
  disconnect(): void {
    this.connection.disconnect();
  }

  /**
   * 检查是否已连接
   */
  get isConnected(): boolean {
    return this.connection.isConnected;
  }

  /**
   * 获取当前会话ID
   */
  get currentSessionId(): string | null {
    return this.connection.currentSessionId;
  }
}
