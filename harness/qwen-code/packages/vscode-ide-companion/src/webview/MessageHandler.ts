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
  // 当前消息列表
  private messages: ChatMessage[] = [];
  // 登录处理器
  private loginHandler?: () => Promise<void>;
  // 待发送消息（登录后自动重发）
  private pendingMessage: string | null = null;

  constructor(
    private agentManager: QwenAgentManager,
    private conversationStore: ConversationStore,
    private currentConversationId: string | null,
    private sendToWebView: (message: unknown) => void,
  ) {}

  /**
   * 设置登录处理器
   */
  setLoginHandler(handler: () => Promise<void>): void {
    this.loginHandler = handler;
  }

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

      case 'focusActiveEditor': {
        // 聚焦到当前激活的编辑器
        const activeEditor = vscode.window.activeTextEditor;
        if (activeEditor) {
          vscode.window.showTextDocument(activeEditor.document, {
            viewColumn: activeEditor.viewColumn,
            preserveFocus: false,
          });
        }
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
        console.log('[MessageHandler] openDiff called with:', data);
        await vscode.commands.executeCommand('qwenCode.showDiff', {
          path: (data as { path?: string })?.path || '',
          oldText: (data as { oldText?: string })?.oldText || '',
          newText: (data as { newText?: string })?.newText || '',
        });
        break;

      case 'openNewChatTab':
        // Create a new WebviewPanel (tab) in the same view column
        await vscode.commands.executeCommand('qwenCode.openNewChatTab');
        break;

      case 'attachFile':
        await this.handleAttachFile();
        break;

      case 'showContextPicker':
        await this.handleShowContextPicker();
        break;

      case 'getWorkspaceFiles':
        await this.handleGetWorkspaceFiles(data?.query as string);
        break;

      case 'saveSession':
        await this.handleSaveSession(data?.tag as string);
        break;

      case 'resumeSession':
        await this.handleResumeSession(data?.sessionId as string);
        break;

      case 'openSettings':
        await this.handleOpenSettings();
        break;

      case 'login':
        await this.handleLogin();
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

    // Check if this is the first message by checking conversation messages
    let isFirstMessage = false;
    try {
      const conversation = await this.conversationStore.getConversation(
        this.currentConversationId,
      );
      // First message if conversation has no messages yet
      isFirstMessage = !conversation || conversation.messages.length === 0;
      console.log('[MessageHandler] Is first message:', isFirstMessage);
    } catch (error) {
      console.error('[MessageHandler] Failed to check conversation:', error);
    }

    // If this is the first message, generate and send session title
    if (isFirstMessage) {
      // Generate title from first message (max 50 characters)
      const title = text.substring(0, 50) + (text.length > 50 ? '...' : '');
      console.log('[MessageHandler] Generated session title:', title);

      // Send title update to WebView
      this.sendToWebView({
        type: 'sessionTitleUpdated',
        data: {
          sessionId: this.currentConversationId,
          title,
        },
      });
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

      // Save pending message for auto-retry after login
      this.pendingMessage = text;
      console.log(
        '[MessageHandler] Saved pending message for retry after login',
      );

      // Show VSCode warning notification
      const result = await vscode.window.showWarningMessage(
        'You need to login first to use Qwen Code.',
        'Login Now',
      );

      if (result === 'Login Now') {
        // Trigger login
        await this.handleLogin();
      }

      // COMMENTED OUT: Send special error type to WebView for inline display
      // console.log('[MessageHandler] Sending notLoggedIn message to webview');
      // this.sendToWebView({
      //   type: 'notLoggedIn',
      //   data: {
      //     message: 'Please login to start chatting with Qwen Code.',
      //   },
      // });
      // console.log('[MessageHandler] notLoggedIn message sent');
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

      // Check if error is due to no active ACP session (not logged in)
      const errorMsg = String(error);
      if (errorMsg.includes('No active ACP session')) {
        // Save pending message for auto-retry after login
        this.pendingMessage = text;
        console.log(
          '[MessageHandler] Saved pending message for retry after login',
        );

        // Show VSCode warning notification with login option
        const result = await vscode.window.showWarningMessage(
          'You need to login first to use Qwen Code.',
          'Login Now',
        );

        if (result === 'Login Now') {
          // Trigger login
          await this.handleLogin();
        }

        // COMMENTED OUT: Send special error type to WebView for inline display with login button
        // console.log('[MessageHandler] Sending notLoggedIn message (session expired) to webview');
        // this.sendToWebView({
        //   type: 'notLoggedIn',
        //   data: {
        //     message: 'Session expired. Please login to continue chatting.',
        //   },
        // });
        // console.log('[MessageHandler] notLoggedIn message sent');
      } else {
        // For other errors, show regular error message
        vscode.window.showErrorMessage(`Error sending message: ${error}`);
        this.sendToWebView({
          type: 'error',
          data: { message: errorMsg },
        });
      }
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

  /**
   * 处理附加文件请求
   * 打开文件选择器，将选中的文件信息发送回WebView
   */
  private async handleAttachFile(): Promise<void> {
    try {
      const uris = await vscode.window.showOpenDialog({
        canSelectMany: false,
        canSelectFiles: true,
        canSelectFolders: false,
        openLabel: 'Attach',
      });

      if (uris && uris.length > 0) {
        const uri = uris[0];
        const fileName = getFileName(uri.fsPath);

        this.sendToWebView({
          type: 'fileAttached',
          data: {
            id: `file-${Date.now()}`,
            type: 'file',
            name: fileName,
            value: uri.fsPath,
          },
        });
      }
    } catch (error) {
      console.error('[MessageHandler] Failed to attach file:', error);
      this.sendToWebView({
        type: 'error',
        data: { message: `Failed to attach file: ${error}` },
      });
    }
  }

  /**
   * 获取工作区文件列表
   * 用于在 @ 触发时显示文件补全
   * 优先显示最近使用的文件（打开的标签页）
   */
  private async handleGetWorkspaceFiles(query?: string): Promise<void> {
    try {
      const files: Array<{
        id: string;
        label: string;
        description: string;
        path: string;
      }> = [];
      const addedPaths = new Set<string>();

      // Helper function to add a file
      const addFile = (uri: vscode.Uri, isCurrentFile = false) => {
        if (addedPaths.has(uri.fsPath)) {
          return;
        }

        const fileName = getFileName(uri.fsPath);
        const workspaceFolder = vscode.workspace.getWorkspaceFolder(uri);
        const relativePath = workspaceFolder
          ? vscode.workspace.asRelativePath(uri, false)
          : uri.fsPath;

        // Filter by query if provided
        if (
          query &&
          !fileName.toLowerCase().includes(query.toLowerCase()) &&
          !relativePath.toLowerCase().includes(query.toLowerCase())
        ) {
          return;
        }

        files.push({
          id: isCurrentFile ? 'current-file' : uri.fsPath,
          label: fileName,
          description: relativePath,
          path: uri.fsPath,
        });
        addedPaths.add(uri.fsPath);
      };

      // If query provided, search entire workspace
      if (query) {
        // Search workspace files matching the query
        const uris = await vscode.workspace.findFiles(
          `**/*${query}*`,
          '**/node_modules/**',
          50, // Allow more results for search
        );

        for (const uri of uris) {
          addFile(uri);
        }
      } else {
        // No query: show recently used files
        // 1. Add current active file first
        const activeEditor = vscode.window.activeTextEditor;
        if (activeEditor) {
          addFile(activeEditor.document.uri, true);
        }

        // 2. Add all open tabs (recently used files)
        const tabGroups = vscode.window.tabGroups.all;
        for (const tabGroup of tabGroups) {
          for (const tab of tabGroup.tabs) {
            const input = tab.input as { uri?: vscode.Uri } | undefined;
            if (input && input.uri instanceof vscode.Uri) {
              addFile(input.uri);
            }
          }
        }

        // 3. If still not enough files (less than 10), add some workspace files
        if (files.length < 10) {
          const recentUris = await vscode.workspace.findFiles(
            '**/*',
            '**/node_modules/**',
            20,
          );

          for (const uri of recentUris) {
            if (files.length >= 20) {
              break;
            }
            addFile(uri);
          }
        }
      }

      this.sendToWebView({
        type: 'workspaceFiles',
        data: { files },
      });
    } catch (error) {
      console.error('[MessageHandler] Failed to get workspace files:', error);
      this.sendToWebView({
        type: 'error',
        data: { message: `Failed to get workspace files: ${error}` },
      });
    }
  }

  /**
   * 处理显示上下文选择器请求
   * 显示快速选择菜单，包含文件、符号等选项
   * 参考 vscode-copilot-chat 的 AttachContextAction
   */
  private async handleShowContextPicker(): Promise<void> {
    try {
      const items: vscode.QuickPickItem[] = [];

      // Add current file
      const activeEditor = vscode.window.activeTextEditor;
      if (activeEditor) {
        const fileName = getFileName(activeEditor.document.uri.fsPath);
        items.push({
          label: `$(file) ${fileName}`,
          description: 'Current file',
          detail: activeEditor.document.uri.fsPath,
        });
      }

      // Add file picker option
      items.push({
        label: '$(file) File...',
        description: 'Choose a file to attach',
      });

      // Add workspace files option
      items.push({
        label: '$(search) Search files...',
        description: 'Search workspace files',
      });

      const selected = await vscode.window.showQuickPick(items, {
        placeHolder: 'Attach context',
        matchOnDescription: true,
        matchOnDetail: true,
      });

      if (selected) {
        if (selected.label.includes('Current file') && activeEditor) {
          const fileName = getFileName(activeEditor.document.uri.fsPath);
          this.sendToWebView({
            type: 'fileAttached',
            data: {
              id: `file-${Date.now()}`,
              type: 'file',
              name: fileName,
              value: activeEditor.document.uri.fsPath,
            },
          });
        } else if (selected.label.includes('File...')) {
          await this.handleAttachFile();
        } else if (selected.label.includes('Search files')) {
          // Open workspace file picker
          const uri = await vscode.window.showOpenDialog({
            defaultUri: vscode.workspace.workspaceFolders?.[0]?.uri,
            canSelectMany: false,
            canSelectFiles: true,
            canSelectFolders: false,
            openLabel: 'Attach',
          });

          if (uri && uri.length > 0) {
            const fileName = getFileName(uri[0].fsPath);
            this.sendToWebView({
              type: 'fileAttached',
              data: {
                id: `file-${Date.now()}`,
                type: 'file',
                name: fileName,
                value: uri[0].fsPath,
              },
            });
          }
        }
      }
    } catch (error) {
      console.error('[MessageHandler] Failed to show context picker:', error);
      this.sendToWebView({
        type: 'error',
        data: { message: `Failed to show context picker: ${error}` },
      });
    }
  }

  /**
   * 处理保存会话请求
   * 首先尝试通过 ACP 协议保存，如果失败则直接保存到文件系统
   */
  private async handleSaveSession(tag: string): Promise<void> {
    try {
      console.log('[MessageHandler] Saving session with tag:', tag);

      if (!this.currentConversationId) {
        throw new Error('No active conversation to save');
      }

      // 从 conversationStore 获取当前会话消息
      const conversation = await this.conversationStore.getConversation(
        this.currentConversationId,
      );
      const messages = conversation?.messages || [];

      // 首先尝试通过 ACP 保存
      try {
        const response = await this.agentManager.saveSessionViaAcp(
          this.currentConversationId,
          tag,
        );

        console.log('[MessageHandler] Session saved via ACP:', response);

        // Send response back to WebView
        this.sendToWebView({
          type: 'saveSessionResponse',
          data: response,
        });
      } catch (acpError) {
        console.warn(
          '[MessageHandler] ACP save failed, falling back to direct save:',
          acpError,
        );

        // ACP 保存失败，尝试直接保存到文件系统
        const response = await this.agentManager.saveSessionDirect(
          messages,
          tag,
        );

        console.log('[MessageHandler] Session saved directly:', response);

        // Send response back to WebView
        this.sendToWebView({
          type: 'saveSessionResponse',
          data: response,
        });
      }

      // Also refresh the session list
      await this.handleGetQwenSessions();
    } catch (error) {
      console.error('[MessageHandler] Failed to save session:', error);
      this.sendToWebView({
        type: 'saveSessionResponse',
        data: {
          success: false,
          message: `Failed to save session: ${error}`,
        },
      });
    }
  }

  /**
   * 处理恢复会话请求
   * 首先尝试通过 ACP 协议加载，如果失败则直接从文件系统加载
   */
  private async handleResumeSession(sessionId: string): Promise<void> {
    try {
      console.log('[MessageHandler] Resuming session:', sessionId);

      // 首先尝试通过 ACP 加载
      try {
        await this.agentManager.loadSessionViaAcp(sessionId);

        // Set current conversation ID
        this.currentConversationId = sessionId;

        // Get session messages for display
        const messages = await this.agentManager.getSessionMessages(sessionId);

        // Send response back to WebView
        this.sendToWebView({
          type: 'qwenSessionSwitched',
          data: { sessionId, messages },
        });
      } catch (acpError) {
        console.warn(
          '[MessageHandler] ACP load failed, falling back to direct load:',
          acpError,
        );

        // ACP 加载失败，尝试直接从文件系统加载
        const messages = await this.agentManager.loadSessionDirect(sessionId);

        if (messages) {
          // Set current conversation ID
          this.currentConversationId = sessionId;

          // Send response back to WebView
          this.sendToWebView({
            type: 'qwenSessionSwitched',
            data: { sessionId, messages },
          });
        } else {
          throw new Error('会话加载失败');
        }
      }

      // Also refresh the session list
      await this.handleGetQwenSessions();
    } catch (error) {
      console.error('[MessageHandler] Failed to resume session:', error);
      this.sendToWebView({
        type: 'error',
        data: { message: `Failed to resume session: ${error}` },
      });
    }
  }

  /**
   * 处理打开设置请求
   * 打开 VSCode 设置页面并定位到扩展配置
   */
  private async handleOpenSettings(): Promise<void> {
    try {
      console.log('[MessageHandler] Opening settings');
      await vscode.commands.executeCommand(
        'workbench.action.openSettings',
        'qwenCode',
      );
    } catch (error) {
      console.error('[MessageHandler] Failed to open settings:', error);
      this.sendToWebView({
        type: 'error',
        data: { message: `Failed to open settings: ${error}` },
      });
    }
  }

  /**
   * 处理登录请求
   * 通过 /login 命令触发登录流程
   */
  private async handleLogin(): Promise<void> {
    try {
      console.log('[MessageHandler] Login requested via /login command');

      if (this.loginHandler) {
        // Show progress notification in VSCode
        await vscode.window.withProgress(
          {
            location: vscode.ProgressLocation.Notification,
            title: 'Logging in to Qwen Code...',
            cancellable: false,
          },
          async () => {
            await this.loginHandler!();
          },
        );
        console.log('[MessageHandler] Login completed successfully');

        // Show success notification
        vscode.window.showInformationMessage(
          'Successfully logged in to Qwen Code!',
        );

        // Auto-resend pending message if exists
        if (this.pendingMessage) {
          console.log(
            '[MessageHandler] Auto-resending pending message after login',
          );
          const messageToSend = this.pendingMessage;
          this.pendingMessage = null; // Clear pending message

          // Resend the message
          await this.handleSendMessage(messageToSend);
        }
      } else {
        console.error('[MessageHandler] No login handler registered');
        this.sendToWebView({
          type: 'error',
          data: { message: 'Login handler not available' },
        });
      }
    } catch (error) {
      console.error('[MessageHandler] Login failed:', error);
      this.sendToWebView({
        type: 'error',
        data: { message: `Login failed: ${error}` },
      });
    }
  }
}
