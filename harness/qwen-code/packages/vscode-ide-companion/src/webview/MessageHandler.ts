/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import type { QwenAgentManager } from '../agents/qwenAgentManager.js';
import type { ConversationStore } from '../storage/conversationStore.js';
import { MessageRouter } from './handlers/MessageRouter.js';

/**
 * MessageHandler (重构版)
 * 这是一个轻量级的包装类，内部使用 MessageRouter 和各个子处理器
 * 保持与原有代码的接口兼容性
 */
export class MessageHandler {
  private router: MessageRouter;

  constructor(
    agentManager: QwenAgentManager,
    conversationStore: ConversationStore,
    currentConversationId: string | null,
    sendToWebView: (message: unknown) => void,
  ) {
    this.router = new MessageRouter(
      agentManager,
      conversationStore,
      currentConversationId,
      sendToWebView,
    );
  }

  /**
   * 路由消息到对应的处理器
   */
  async route(message: { type: string; data?: unknown }): Promise<void> {
    await this.router.route(message);
  }

  /**
   * 设置当前会话 ID
   */
  setCurrentConversationId(id: string | null): void {
    this.router.setCurrentConversationId(id);
  }

  /**
   * 获取当前会话 ID
   */
  getCurrentConversationId(): string | null {
    return this.router.getCurrentConversationId();
  }

  /**
   * 设置权限处理器
   */
  setPermissionHandler(
    handler: (message: { type: string; data: { optionId: string } }) => void,
  ): void {
    this.router.setPermissionHandler(handler);
  }

  /**
   * 设置登录处理器
   */
  setLoginHandler(handler: () => Promise<void>): void {
    this.router.setLoginHandler(handler);
  }

  /**
   * 追加流式内容
   */
  appendStreamContent(chunk: string): void {
    this.router.appendStreamContent(chunk);
  }

  /**
   * 检查是否正在保存 checkpoint
   */
  getIsSavingCheckpoint(): boolean {
    return this.router.getIsSavingCheckpoint();
  }
}
