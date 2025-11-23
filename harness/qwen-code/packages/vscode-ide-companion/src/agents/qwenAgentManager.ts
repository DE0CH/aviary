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
   * 通过发送 /chat save 命令保存会话
   * 由于 CLI 不支持 session/save ACP 方法，我们直接发送 /chat save 命令
   *
   * @param sessionId - 会话ID
   * @param tag - 保存标签
   * @returns 保存响应
   */
  async saveSessionViaCommand(
    sessionId: string,
    tag: string,
  ): Promise<{ success: boolean; message?: string }> {
    try {
      console.log(
        '[QwenAgentManager] Saving session via /chat save command:',
        sessionId,
        'with tag:',
        tag,
      );

      // Send /chat save command as a prompt
      // The CLI will handle this as a special command
      await this.connection.sendPrompt(`/chat save "${tag}"`);

      console.log('[QwenAgentManager] /chat save command sent successfully');
      return {
        success: true,
        message: `Session saved with tag: ${tag}`,
      };
    } catch (error) {
      console.error('[QwenAgentManager] /chat save command failed:', error);
      return {
        success: false,
        message: error instanceof Error ? error.message : String(error),
      };
    }
  }

  /**
   * 通过 ACP session/save 方法保存会话 (已废弃，CLI 不支持)
   *
   * @deprecated Use saveSessionViaCommand instead
   * @param sessionId - 会话ID
   * @param tag - 保存标签
   * @returns 保存响应
   */
  async saveSessionViaAcp(
    sessionId: string,
    tag: string,
  ): Promise<{ success: boolean; message?: string }> {
    // Fallback to command-based save since CLI doesn't support session/save ACP method
    console.warn(
      '[QwenAgentManager] saveSessionViaAcp is deprecated, using command-based save instead',
    );
    return this.saveSessionViaCommand(sessionId, tag);
  }

  /**
   * 通过发送 /chat save 命令保存会话（CLI 方式）
   * 这会调用 CLI 的原生保存功能，确保保存的内容完整
   *
   * @param tag - Checkpoint 标签
   * @returns 保存结果
   */
  async saveCheckpointViaCommand(
    tag: string,
  ): Promise<{ success: boolean; tag?: string; message?: string }> {
    try {
      console.log(
        '[QwenAgentManager] ===== SAVING VIA /chat save COMMAND =====',
      );
      console.log('[QwenAgentManager] Tag:', tag);

      // Send /chat save command as a prompt
      // The CLI will handle this as a special command and save the checkpoint
      const command = `/chat save "${tag}"`;
      console.log('[QwenAgentManager] Sending command:', command);

      await this.connection.sendPrompt(command);

      console.log(
        '[QwenAgentManager] Command sent, checkpoint should be saved by CLI',
      );

      // Wait a bit for CLI to process the command
      await new Promise((resolve) => setTimeout(resolve, 500));

      return {
        success: true,
        tag,
        message: `Checkpoint saved via CLI: ${tag}`,
      };
    } catch (error) {
      console.error('[QwenAgentManager] /chat save command failed:', error);
      return {
        success: false,
        message: error instanceof Error ? error.message : String(error),
      };
    }
  }

  /**
   * 保存会话为 checkpoint（使用 CLI 的格式）
   * 保存到 ~/.qwen/tmp/{projectHash}/checkpoint-{tag}.json
   * 同时用 sessionId 和 conversationId 保存两份，确保可以通过任一 ID 恢复
   *
   * @param messages - 当前会话消息
   * @param conversationId - Conversation ID (from VSCode extension)
   * @returns 保存结果
   */
  async saveCheckpoint(
    messages: ChatMessage[],
    conversationId: string,
  ): Promise<{ success: boolean; tag?: string; message?: string }> {
    try {
      console.log('[QwenAgentManager] ===== CHECKPOINT SAVE START =====');
      console.log('[QwenAgentManager] Conversation ID:', conversationId);
      console.log('[QwenAgentManager] Message count:', messages.length);
      console.log(
        '[QwenAgentManager] Current working dir:',
        this.currentWorkingDir,
      );
      console.log(
        '[QwenAgentManager] Current session ID (from CLI):',
        this.currentSessionId,
      );

      // Use CLI's /chat save command instead of manually writing files
      // This ensures we save the complete session context including tool calls
      if (this.currentSessionId) {
        console.log(
          '[QwenAgentManager] Using CLI /chat save command for complete save',
        );
        return await this.saveCheckpointViaCommand(this.currentSessionId);
      } else {
        console.warn(
          '[QwenAgentManager] No current session ID, cannot use /chat save',
        );
        return {
          success: false,
          message: 'No active CLI session',
        };
      }
    } catch (error) {
      console.error('[QwenAgentManager] ===== CHECKPOINT SAVE FAILED =====');
      console.error('[QwenAgentManager] Error:', error);
      console.error(
        '[QwenAgentManager] Error stack:',
        error instanceof Error ? error.stack : 'N/A',
      );
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
    // Use checkpoint format instead of session format
    // This matches CLI's /chat save behavior
    return this.saveCheckpoint(messages, sessionName);
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
        '[QwenAgentManager] Attempting session/load via ACP for session:',
        sessionId,
      );
      const response = await this.connection.loadSession(sessionId);
      console.log(
        '[QwenAgentManager] Session load succeeded. Response:',
        JSON.stringify(response).substring(0, 200),
      );
      return response;
    } catch (error) {
      const errorMessage =
        error instanceof Error ? error.message : String(error);
      console.error(
        '[QwenAgentManager] Session load via ACP failed for session:',
        sessionId,
      );
      console.error('[QwenAgentManager] Error type:', error?.constructor?.name);
      console.error('[QwenAgentManager] Error message:', errorMessage);

      // Check if error is from ACP response
      if (error && typeof error === 'object' && 'error' in error) {
        const acpError = error as {
          error?: { code?: number; message?: string };
        };
        if (acpError.error) {
          console.error(
            '[QwenAgentManager] ACP error code:',
            acpError.error.code,
          );
          console.error(
            '[QwenAgentManager] ACP error message:',
            acpError.error.message,
          );
        }
      }

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
