/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import * as vscode from 'vscode';
import type { QwenAgentManager } from '../agents/qwenAgentManager.js';
import { type ChatMessage } from '../agents/qwenAgentManager.js';
import type { ConversationStore } from '../storage/conversationStore.js';
import { FileOperations } from './FileOperations.js';
import { CliInstaller } from './CliInstaller.js';
import { CliDetector } from '../utils/cliDetector.js';
import { getFileName } from '../utils/webviewUtils.js';

/**
 * WebView 消息处理器
 * 负责处理所有来自 WebView 的消息请求
 */
export class MessageHandler {
  // 跟踪当前流式内容，用于保存
  private currentStreamContent = '';
  // 权限请求处理器（临时存储）
  private permissionHandler?: (msg: {
    type: string;
    data: { optionId: string };
  }) => void;

  constructor(
    private agentManager: QwenAgentManager,
    private conversationStore: ConversationStore,
    private currentConversationId: string | null,
    private sendToWebView: (message: unknown) => void,
  ) {}

  /**
   * 获取当前对话 ID
   */
  getCurrentConversationId(): string | null {
    return this.currentConversationId;
  }

  /**
   * 设置当前对话 ID
   */
  setCurrentConversationId(id: string | null): void {
    this.currentConversationId = id;
  }

  /**
   * 获取当前流式内容
   */
  getCurrentStreamContent(): string {
    return this.currentStreamContent;
  }

  /**
   * 追加流式内容
   */
  appendStreamContent(chunk: string): void {
    this.currentStreamContent += chunk;
  }

  /**
   * 重置流式内容
   */
  resetStreamContent(): void {
    this.currentStreamContent = '';
  }

  /**
   * 设置权限处理器
   */
  setPermissionHandler(
    handler: (msg: { type: string; data: { optionId: string } }) => void,
  ): void {
    this.permissionHandler = handler;
  }

  /**
   * 清除权限处理器
   */
  clearPermissionHandler(): void {
    this.permissionHandler = undefined;
  }

  /**
   * 路由 WebView 消息到对应的处理函数
   */
  async route(message: { type: string; data?: unknown }): Promise<void> {
    console.log('[MessageHandler] Received message from webview:', message);

    // Type guard for safe access to data properties
    const data = message.data as Record<string, unknown> | undefined;

    switch (message.type) {
      case 'sendMessage':
        await this.handleSendMessage((data?.text as string) || '');
        break;

      case 'permissionResponse':
        // Forward to permission handler
        if (this.permissionHandler) {
          this.permissionHandler(
            message as { type: string; data: { optionId: string } },
          );
          this.clearPermissionHandler();
        }
        break;

      case 'loadConversation':
        await this.handleLoadConversation((data?.id as string) || '');
        break;

      case 'newConversation':
        await this.handleNewConversation();
        break;

      case 'newQwenSession':
        await this.handleNewQwenSession();
        break;

      case 'deleteConversation':
        await this.handleDeleteConversation((data?.id as string) || '');
        break;

      case 'getQwenSessions':
        await this.handleGetQwenSessions();
        break;

      case 'getActiveEditor': {
        // 发送当前激活编辑器的文件名给 WebView
        const editor = vscode.window.activeTextEditor;
        const fileName = editor?.document.uri.fsPath
          ? getFileName(editor.document.uri.fsPath)
          : null;
        this.sendToWebView({
          type: 'activeEditorChanged',
          data: { fileName },
        });
        break;
      }

      case 'switchQwenSession':
        await this.handleSwitchQwenSession((data?.sessionId as string) || '');
        break;

      case 'recheckCli':
        // Clear cache and recheck CLI installation
        CliDetector.clearCache();
        await CliInstaller.checkInstallation(this.sendToWebView);
        break;

      case 'cancelPrompt':
        await this.handleCancelPrompt();
        break;

      case 'openFile':
        await FileOperations.openFile(data?.path as string | undefined);
        break;

      case 'openDiff':
        await FileOperations.openDiff(
          data as {
            path?: string;
            oldText?: string;
            newText?: string;
          },
        );
        break;

      case 'openNewChatTab':
        // Create a new session in the current panel instead of opening a new panel
        await this.handleNewQwenSession();
        break;

      default:
        console.warn('[MessageHandler] Unknown message type:', message.type);
        break;
    }
  }

  /**
   * 处理发送消息请求
   */
  private async handleSendMessage(text: string): Promise<void> {
    console.log('[MessageHandler] handleSendMessage called with:', text);

    // Ensure we have an active conversation - create one if needed
    if (!this.currentConversationId) {
      console.log('[MessageHandler] No active conversation, creating one...');
      try {
        const newConv = await this.conversationStore.createConversation();
        this.currentConversationId = newConv.id;
        this.sendToWebView({
          type: 'conversationLoaded',
          data: newConv,
        });
        console.log(
          '[MessageHandler] Created conversation:',
          this.currentConversationId,
        );
      } catch (error) {
        const errorMsg = `Failed to create conversation: ${error}`;
        console.error('[MessageHandler]', errorMsg);
        vscode.window.showErrorMessage(errorMsg);
        this.sendToWebView({
          type: 'error',
          data: { message: errorMsg },
        });
        return;
      }
    }

    // Double check after creation attempt
    if (!this.currentConversationId) {
      const errorMsg =
        'Failed to create conversation. Please restart the extension.';
      console.error('[MessageHandler]', errorMsg);
      vscode.window.showErrorMessage(errorMsg);
      this.sendToWebView({
        type: 'error',
        data: { message: errorMsg },
      });
      return;
    }

    // Save user message
    const userMessage: ChatMessage = {
      role: 'user',
      content: text,
      timestamp: Date.now(),
    };

    await this.conversationStore.addMessage(
      this.currentConversationId,
      userMessage,
    );
    console.log('[MessageHandler] User message saved to store');

    // Send to WebView
    this.sendToWebView({
      type: 'message',
      data: userMessage,
    });
    console.log('[MessageHandler] User message sent to webview');

    // Check if agent is connected
    if (!this.agentManager.isConnected) {
      console.warn(
        '[MessageHandler] Agent is not connected, skipping AI response',
      );
      this.sendToWebView({
        type: 'error',
        data: {
          message:
            'Agent is not connected. Enable Qwen in settings or configure API key.',
        },
      });
      return;
    }

    // Send to agent
    try {
      // Reset stream content
      this.resetStreamContent();

      // Create placeholder for assistant message
      this.sendToWebView({
        type: 'streamStart',
        data: { timestamp: Date.now() },
      });
      console.log('[MessageHandler] Stream start sent');

      console.log('[MessageHandler] Sending to agent manager...');
      await this.agentManager.sendMessage(text);
      console.log('[MessageHandler] Agent manager send complete');

      // Stream is complete - save assistant message
      if (this.currentStreamContent && this.currentConversationId) {
        const assistantMessage: ChatMessage = {
          role: 'assistant',
          content: this.currentStreamContent,
          timestamp: Date.now(),
        };
        await this.conversationStore.addMessage(
          this.currentConversationId,
          assistantMessage,
        );
        console.log('[MessageHandler] Assistant message saved to store');
      }

      this.sendToWebView({
        type: 'streamEnd',
        data: { timestamp: Date.now() },
      });
      console.log('[MessageHandler] Stream end sent');
    } catch (error) {
      console.error('[MessageHandler] Error sending message:', error);
      vscode.window.showErrorMessage(`Error sending message: ${error}`);
      this.sendToWebView({
        type: 'error',
        data: { message: String(error) },
      });
    }
  }

  /**
   * 处理加载对话请求
   */
  private async handleLoadConversation(id: string): Promise<void> {
    const conversation = await this.conversationStore.getConversation(id);
    if (conversation) {
      this.currentConversationId = id;
      this.sendToWebView({
        type: 'conversationLoaded',
        data: conversation,
      });
    }
  }

  /**
   * 处理新建对话请求
   */
  private async handleNewConversation(): Promise<void> {
    const newConv = await this.conversationStore.createConversation();
    this.currentConversationId = newConv.id;
    this.sendToWebView({
      type: 'conversationLoaded',
      data: newConv,
    });
  }

  /**
   * 处理删除对话请求
   */
  private async handleDeleteConversation(id: string): Promise<void> {
    await this.conversationStore.deleteConversation(id);
    this.sendToWebView({
      type: 'conversationDeleted',
      data: { id },
    });
  }

  /**
   * 处理获取 Qwen 会话列表请求
   */
  private async handleGetQwenSessions(): Promise<void> {
    try {
      console.log('[MessageHandler] Getting Qwen sessions...');
      const sessions = await this.agentManager.getSessionList();
      console.log('[MessageHandler] Retrieved sessions:', sessions.length);

      this.sendToWebView({
        type: 'qwenSessionList',
        data: { sessions },
      });
    } catch (error) {
      console.error('[MessageHandler] Failed to get Qwen sessions:', error);
      this.sendToWebView({
        type: 'error',
        data: { message: `Failed to get sessions: ${error}` },
      });
    }
  }

  /**
   * 处理新建 Qwen 会话请求
   */
  private async handleNewQwenSession(): Promise<void> {
    try {
      console.log('[MessageHandler] Creating new Qwen session...');
      const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
      const workingDir = workspaceFolder?.uri.fsPath || process.cwd();

      await this.agentManager.createNewSession(workingDir);

      // Clear current conversation UI
      this.sendToWebView({
        type: 'conversationCleared',
        data: {},
      });
    } catch (error) {
      console.error('[MessageHandler] Failed to create new session:', error);
      this.sendToWebView({
        type: 'error',
        data: { message: `Failed to create new session: ${error}` },
      });
    }
  }

  /**
   * 处理切换 Qwen 会话请求
   */
  private async handleSwitchQwenSession(sessionId: string): Promise<void> {
    try {
      console.log('[MessageHandler] Switching to Qwen session:', sessionId);

      // Get session messages from local files
      const messages = await this.agentManager.getSessionMessages(sessionId);
      console.log(
        '[MessageHandler] Loaded messages from session:',
        messages.length,
      );

      // Get session details for the header
      let sessionDetails = null;
      try {
        const allSessions = await this.agentManager.getSessionList();
        sessionDetails = allSessions.find(
          (s: { id?: string; sessionId?: string }) =>
            s.id === sessionId || s.sessionId === sessionId,
        );
      } catch (err) {
        console.log('[MessageHandler] Could not get session details:', err);
      }

      // Try to load session via ACP first, fallback to creating new session
      const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
      const workingDir = workspaceFolder?.uri.fsPath || process.cwd();

      try {
        console.log('[MessageHandler] Testing session/load via ACP...');
        const loadResponse =
          await this.agentManager.loadSessionViaAcp(sessionId);
        console.log('[MessageHandler] session/load succeeded:', loadResponse);

        // If load succeeded, use the loaded session
        this.currentConversationId = sessionId;
        console.log(
          '[MessageHandler] Set currentConversationId to loaded session:',
          sessionId,
        );
      } catch (_loadError) {
        console.log(
          '[MessageHandler] session/load not supported, creating new session',
        );

        // Fallback: CLI doesn't support loading old sessions
        // So we create a NEW ACP session for continuation
        try {
          const newAcpSessionId =
            await this.agentManager.createNewSession(workingDir);
          console.log(
            '[MessageHandler] Created new ACP session for conversation:',
            newAcpSessionId,
          );

          // Use the NEW ACP session ID for sending messages to CLI
          this.currentConversationId = newAcpSessionId;
          console.log(
            '[MessageHandler] Set currentConversationId (ACP) to:',
            newAcpSessionId,
          );
        } catch (createError) {
          console.error(
            '[MessageHandler] Failed to create new ACP session:',
            createError,
          );
          vscode.window.showWarningMessage(
            'Could not switch to session. Created new session instead.',
          );
          throw createError;
        }
      }

      // Send messages and session details to WebView
      // The historical messages are display-only, not sent to CLI
      this.sendToWebView({
        type: 'qwenSessionSwitched',
        data: { sessionId, messages, session: sessionDetails },
      });
    } catch (error) {
      console.error('[MessageHandler] Failed to switch session:', error);
      this.sendToWebView({
        type: 'error',
        data: { message: `Failed to switch session: ${error}` },
      });
      vscode.window.showErrorMessage(`Failed to switch session: ${error}`);
    }
  }

  /**
   * 处理取消提示请求
   * 取消当前 AI 响应生成
   */
  private async handleCancelPrompt(): Promise<void> {
    try {
      console.log('[MessageHandler] Cancel prompt requested');

      if (!this.agentManager.isConnected) {
        console.warn('[MessageHandler] Agent not connected, cannot cancel');
        return;
      }

      await this.agentManager.cancelCurrentPrompt();

      this.sendToWebView({
        type: 'promptCancelled',
        data: { timestamp: Date.now() },
      });

      console.log('[MessageHandler] Prompt cancelled successfully');
    } catch (error) {
      console.error('[MessageHandler] Failed to cancel prompt:', error);
      this.sendToWebView({
        type: 'error',
        data: { message: `Failed to cancel: ${error}` },
      });
    }
  }
}
