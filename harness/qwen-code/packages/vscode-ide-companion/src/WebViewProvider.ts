/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import * as vscode from 'vscode';
import {
  QwenAgentManager,
  type ChatMessage,
} from './agents/qwenAgentManager.js';
import { ConversationStore } from './storage/conversationStore.js';
import type { AcpPermissionRequest } from './shared/acpTypes.js';
import { CliDetector } from './utils/cliDetector.js';
import { AuthStateManager } from './auth/authStateManager.js';

export class WebViewProvider {
  private panel: vscode.WebviewPanel | null = null;
  // Track the Webview tab (avoid pin/lock; use for reveal/visibility bookkeeping)
  private panelTab: vscode.Tab | null = null;
  private agentManager: QwenAgentManager;
  private conversationStore: ConversationStore;
  private authStateManager: AuthStateManager;
  private currentConversationId: string | null = null;
  private disposables: vscode.Disposable[] = [];
  private agentInitialized = false; // Track if agent has been initialized
  private currentStreamContent = ''; // Track streaming content for saving

  constructor(
    context: vscode.ExtensionContext,
    private extensionUri: vscode.Uri,
  ) {
    this.agentManager = new QwenAgentManager();
    this.conversationStore = new ConversationStore(context);
    this.authStateManager = new AuthStateManager(context);

    // Setup agent callbacks
    this.agentManager.onStreamChunk((chunk: string) => {
      this.currentStreamContent += chunk;
      this.sendMessageToWebView({
        type: 'streamChunk',
        data: { chunk },
      });
    });

    // Setup thought chunk handler
    this.agentManager.onThoughtChunk((chunk: string) => {
      this.currentStreamContent += chunk;
      this.sendMessageToWebView({
        type: 'thoughtChunk',
        data: { chunk },
      });
    });

    // Note: Tool call updates are handled in handleSessionUpdate within QwenAgentManager
    // and sent via onStreamChunk callback
    this.agentManager.onToolCall((update) => {
      this.sendMessageToWebView({
        type: 'toolCall',
        data: {
          type: 'tool_call',
          ...(update as unknown as Record<string, unknown>),
        },
      });
    });

    // Setup plan handler
    this.agentManager.onPlan((entries) => {
      this.sendMessageToWebView({
        type: 'plan',
        data: { entries },
      });
    });

    this.agentManager.onPermissionRequest(
      async (request: AcpPermissionRequest) => {
        // Send permission request to WebView
        this.sendMessageToWebView({
          type: 'permissionRequest',
          data: request,
        });

        // Wait for user response
        return new Promise((resolve) => {
          const handler = (message: {
            type: string;
            data: { optionId: string };
          }) => {
            if (message.type === 'permissionResponse') {
              resolve(message.data.optionId);
            }
          };
          // Store handler temporarily (in real implementation, use proper event system)
          (this as { permissionHandler?: typeof handler }).permissionHandler =
            handler;
        });
      },
    );
  }

  async show(): Promise<void> {
    // Track if we're creating a new panel in a new column
    let startedInNewColumn = false;

    if (this.panel) {
      // Reveal the existing panel via Tab API (Claude-style), fallback to panel.reveal
      this.revealPanelTab(true);
      this.capturePanelTab();
      return;
    }

    // Mark that we're creating a new panel
    startedInNewColumn = true;

    this.panel = vscode.window.createWebviewPanel(
      'qwenCode.chat',
      'Qwen Code Chat',
      {
        viewColumn: vscode.ViewColumn.Beside, // Open on right side of active editor
        preserveFocus: true, // Don't steal focus from editor
      },
      {
        enableScripts: true,
        retainContextWhenHidden: true,
        localResourceRoots: [
          vscode.Uri.joinPath(this.extensionUri, 'dist'),
          vscode.Uri.joinPath(this.extensionUri, 'assets'),
        ],
      },
    );

    // Capture the Tab that corresponds to our WebviewPanel (Claude-style)
    this.capturePanelTab();

    // Auto-lock editor group when opened in new column (Claude Code style)
    if (startedInNewColumn) {
      console.log(
        '[WebViewProvider] Auto-locking editor group for Qwen Code chat',
      );
      try {
        // Reveal panel without preserving focus to make it the active group
        // This ensures the lock command targets the correct editor group
        this.revealPanelTab(false);

        await vscode.commands.executeCommand(
          'workbench.action.lockEditorGroup',
        );
        console.log('[WebViewProvider] Editor group locked successfully');
      } catch (error) {
        console.warn('[WebViewProvider] Failed to lock editor group:', error);
        // Non-fatal error, continue anyway
      }
    } else {
      // For existing panel, reveal with preserving focus
      this.revealPanelTab(true);
    }

    // Set panel icon to Qwen logo
    this.panel.iconPath = vscode.Uri.joinPath(
      this.extensionUri,
      'assets',
      'icon.png',
    );

    this.panel.webview.html = this.getWebviewContent();

    // Handle messages from WebView
    this.panel.webview.onDidReceiveMessage(
      async (message) => {
        await this.handleWebViewMessage(message);
      },
      null,
      this.disposables,
    );

    // Listen for view state changes (no pin/lock; just keep tab reference fresh)
    this.panel.onDidChangeViewState(
      () => {
        if (this.panel && this.panel.visible) {
          this.capturePanelTab();
        }
      },
      null,
      this.disposables,
    );

    this.panel.onDidDispose(
      () => {
        this.panel = null;
        // Don't disconnect agent - keep it alive for next time
        this.disposables.forEach((d) => d.dispose());
      },
      null,
      this.disposables,
    );

    // Initialize agent connection only once
    if (!this.agentInitialized) {
      await this.initializeAgentConnection();
    } else {
      console.log(
        '[WebViewProvider] Agent already initialized, reusing existing connection',
      );
      // Reload current session messages
      await this.loadCurrentSessionMessages();
    }
  }

  /**
   * Initialize agent connection and session
   * Can be called from show() or restorePanel()
   */
  private async initializeAgentConnection(): Promise<void> {
    const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
    const workingDir = workspaceFolder?.uri.fsPath || process.cwd();

    console.log(
      '[WebViewProvider] Starting initialization, workingDir:',
      workingDir,
    );

    const config = vscode.workspace.getConfiguration('qwenCode');
    const qwenEnabled = config.get<boolean>('qwen.enabled', true);

    if (qwenEnabled) {
      // Check if CLI is installed before attempting to connect
      const cliDetection = await CliDetector.detectQwenCli();

      if (!cliDetection.isInstalled) {
        console.log(
          '[WebViewProvider] Qwen CLI not detected, skipping agent connection',
        );
        console.log(
          '[WebViewProvider] CLI detection error:',
          cliDetection.error,
        );

        // Show VSCode notification with installation option
        await this.promptCliInstallation();

        // Initialize empty conversation (can still browse history)
        await this.initializeEmptyConversation();
      } else {
        console.log(
          '[WebViewProvider] Qwen CLI detected, attempting connection...',
        );
        console.log('[WebViewProvider] CLI path:', cliDetection.cliPath);
        console.log('[WebViewProvider] CLI version:', cliDetection.version);

        try {
          console.log('[WebViewProvider] Connecting to agent...');
          const authInfo = await this.authStateManager.getAuthInfo();
          console.log('[WebViewProvider] Auth cache status:', authInfo);

          await this.agentManager.connect(workingDir, this.authStateManager);
          console.log('[WebViewProvider] Agent connected successfully');
          this.agentInitialized = true;

          // Load messages from the current Qwen session
          await this.loadCurrentSessionMessages();
        } catch (error) {
          console.error('[WebViewProvider] Agent connection error:', error);
          // Clear auth cache on error (might be auth issue)
          await this.authStateManager.clearAuthState();
          vscode.window.showWarningMessage(
            `Failed to connect to Qwen CLI: ${error}\nYou can still use the chat UI, but messages won't be sent to AI.`,
          );
          // Fallback to empty conversation
          await this.initializeEmptyConversation();
        }
      }
    } else {
      console.log('[WebViewProvider] Qwen agent is disabled in settings');
      // Fallback to ConversationStore
      await this.initializeEmptyConversation();
    }
  }

  private async checkCliInstallation(): Promise<void> {
    try {
      const result = await CliDetector.detectQwenCli();

      this.sendMessageToWebView({
        type: 'cliDetectionResult',
        data: {
          isInstalled: result.isInstalled,
          cliPath: result.cliPath,
          version: result.version,
          error: result.error,
          installInstructions: result.isInstalled
            ? undefined
            : CliDetector.getInstallationInstructions(),
        },
      });

      if (!result.isInstalled) {
        console.log('[WebViewProvider] Qwen CLI not detected:', result.error);
      } else {
        console.log(
          '[WebViewProvider] Qwen CLI detected:',
          result.cliPath,
          result.version,
        );
      }
    } catch (error) {
      console.error('[WebViewProvider] CLI detection error:', error);
    }
  }

  private async loadCurrentSessionMessages(): Promise<void> {
    try {
      console.log(
        '[WebViewProvider] Initializing with empty conversation and creating ACP session',
      );

      // Create a new ACP session so user can send messages immediately
      const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
      const workingDir = workspaceFolder?.uri.fsPath || process.cwd();

      try {
        await this.agentManager.createNewSession(workingDir);
        console.log('[WebViewProvider] ACP session created successfully');
      } catch (sessionError) {
        console.error(
          '[WebViewProvider] Failed to create ACP session:',
          sessionError,
        );
        vscode.window.showWarningMessage(
          `Failed to create ACP session: ${sessionError}. You may need to authenticate first.`,
        );
      }

      await this.initializeEmptyConversation();
    } catch (error) {
      console.error(
        '[WebViewProvider] Failed to load session messages:',
        error,
      );
      vscode.window.showErrorMessage(
        `Failed to load session messages: ${error}`,
      );
      await this.initializeEmptyConversation();
    }
  }

  private async promptCliInstallation(): Promise<void> {
    const selection = await vscode.window.showWarningMessage(
      'Qwen Code CLI is not installed. You can browse conversation history, but cannot send new messages.',
      'Install Now',
      'View Documentation',
      'Remind Me Later',
    );

    if (selection === 'Install Now') {
      await this.installQwenCli();
    } else if (selection === 'View Documentation') {
      vscode.env.openExternal(
        vscode.Uri.parse('https://github.com/QwenLM/qwen-code#installation'),
      );
    }
  }

  private async installQwenCli(): Promise<void> {
    try {
      // Show progress notification
      await vscode.window.withProgress(
        {
          location: vscode.ProgressLocation.Notification,
          title: 'Installing Qwen Code CLI',
          cancellable: false,
        },
        async (progress) => {
          progress.report({
            message: 'Running: npm install -g @qwen-code/qwen-code@latest',
          });

          const { exec } = await import('child_process');
          const { promisify } = await import('util');
          const execAsync = promisify(exec);

          try {
            const { stdout, stderr } = await execAsync(
              'npm install -g @qwen-code/qwen-code@latest',
              { timeout: 120000 }, // 2 minutes timeout
            );

            console.log('[WebViewProvider] Installation output:', stdout);
            if (stderr) {
              console.warn('[WebViewProvider] Installation stderr:', stderr);
            }

            // Clear cache and recheck
            CliDetector.clearCache();
            const detection = await CliDetector.detectQwenCli();

            if (detection.isInstalled) {
              vscode.window
                .showInformationMessage(
                  `✅ Qwen Code CLI installed successfully! Version: ${detection.version}`,
                  'Reload Window',
                )
                .then((selection) => {
                  if (selection === 'Reload Window') {
                    vscode.commands.executeCommand(
                      'workbench.action.reloadWindow',
                    );
                  }
                });

              // Update webview with new detection result
              await this.checkCliInstallation();
            } else {
              throw new Error(
                'Installation completed but CLI still not detected',
              );
            }
          } catch (error) {
            const errorMessage =
              error instanceof Error ? error.message : String(error);
            console.error(
              '[WebViewProvider] Installation failed:',
              errorMessage,
            );

            vscode.window
              .showErrorMessage(
                `Failed to install Qwen Code CLI: ${errorMessage}`,
                'Try Manual Installation',
                'View Documentation',
              )
              .then((selection) => {
                if (selection === 'Try Manual Installation') {
                  const terminal = vscode.window.createTerminal(
                    'Qwen Code Installation',
                  );
                  terminal.show();
                  terminal.sendText(
                    'npm install -g @qwen-code/qwen-code@latest',
                  );
                } else if (selection === 'View Documentation') {
                  vscode.env.openExternal(
                    vscode.Uri.parse(
                      'https://github.com/QwenLM/qwen-code#installation',
                    ),
                  );
                }
              });
          }
        },
      );
    } catch (error) {
      console.error('[WebViewProvider] Install CLI error:', error);
    }
  }

  private async initializeEmptyConversation(): Promise<void> {
    try {
      console.log('[WebViewProvider] Initializing empty conversation');
      const newConv = await this.conversationStore.createConversation();
      this.currentConversationId = newConv.id;
      this.sendMessageToWebView({
        type: 'conversationLoaded',
        data: newConv,
      });
      console.log(
        '[WebViewProvider] Empty conversation initialized:',
        this.currentConversationId,
      );
    } catch (error) {
      console.error(
        '[WebViewProvider] Failed to initialize conversation:',
        error,
      );
      // Send empty state to WebView as fallback
      this.sendMessageToWebView({
        type: 'conversationLoaded',
        data: { id: 'temp', messages: [] },
      });
    }
  }

  private async handleWebViewMessage(message: {
    type: string;
    data?: { text?: string; id?: string; sessionId?: string };
  }): Promise<void> {
    console.log('[WebViewProvider] Received message from webview:', message);
    const self = this as {
      permissionHandler?: (msg: {
        type: string;
        data: { optionId: string };
      }) => void;
    };
    switch (message.type) {
      case 'sendMessage':
        await this.handleSendMessage(message.data?.text || '');
        break;

      case 'permissionResponse':
        // Forward to permission handler
        if (self.permissionHandler) {
          self.permissionHandler(
            message as { type: string; data: { optionId: string } },
          );
          delete self.permissionHandler;
        }
        break;

      case 'loadConversation':
        await this.handleLoadConversation(message.data?.id || '');
        break;

      case 'newConversation':
        await this.handleNewConversation();
        break;

      case 'newQwenSession':
        await this.handleNewQwenSession();
        break;

      case 'deleteConversation':
        await this.handleDeleteConversation(message.data?.id || '');
        break;

      case 'getQwenSessions':
        await this.handleGetQwenSessions();
        break;

      case 'switchQwenSession':
        await this.handleSwitchQwenSession(message.data?.sessionId || '');
        break;

      case 'recheckCli':
        // Clear cache and recheck CLI installation
        CliDetector.clearCache();
        await this.checkCliInstallation();
        break;

      case 'cancelPrompt':
        await this.handleCancelPrompt();
        break;

      default:
        console.warn('[WebViewProvider] Unknown message type:', message.type);
        break;
    }
  }

  private async handleSendMessage(text: string): Promise<void> {
    console.log('[WebViewProvider] handleSendMessage called with:', text);

    // Ensure we have an active conversation - create one if needed
    if (!this.currentConversationId) {
      console.log('[WebViewProvider] No active conversation, creating one...');
      try {
        await this.initializeEmptyConversation();
        console.log(
          '[WebViewProvider] Created conversation:',
          this.currentConversationId,
        );
      } catch (error) {
        const errorMsg = `Failed to create conversation: ${error}`;
        console.error('[WebViewProvider]', errorMsg);
        vscode.window.showErrorMessage(errorMsg);
        this.sendMessageToWebView({
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
      console.error('[WebViewProvider]', errorMsg);
      vscode.window.showErrorMessage(errorMsg);
      this.sendMessageToWebView({
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
    console.log('[WebViewProvider] User message saved to store');

    // Send to WebView
    this.sendMessageToWebView({
      type: 'message',
      data: userMessage,
    });
    console.log('[WebViewProvider] User message sent to webview');

    // Check if agent is connected
    if (!this.agentManager.isConnected) {
      console.warn(
        '[WebViewProvider] Agent is not connected, skipping AI response',
      );
      this.sendMessageToWebView({
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
      this.currentStreamContent = '';

      // Create placeholder for assistant message
      this.sendMessageToWebView({
        type: 'streamStart',
        data: { timestamp: Date.now() },
      });
      console.log('[WebViewProvider] Stream start sent');

      console.log('[WebViewProvider] Sending to agent manager...');
      await this.agentManager.sendMessage(text);
      console.log('[WebViewProvider] Agent manager send complete');

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
        console.log('[WebViewProvider] Assistant message saved to store');
      }

      this.sendMessageToWebView({
        type: 'streamEnd',
        data: { timestamp: Date.now() },
      });
      console.log('[WebViewProvider] Stream end sent');
    } catch (error) {
      console.error('[WebViewProvider] Error sending message:', error);
      vscode.window.showErrorMessage(`Error sending message: ${error}`);
      this.sendMessageToWebView({
        type: 'error',
        data: { message: String(error) },
      });
    }
  }

  private async handleLoadConversation(id: string): Promise<void> {
    const conversation = await this.conversationStore.getConversation(id);
    if (conversation) {
      this.currentConversationId = id;
      this.sendMessageToWebView({
        type: 'conversationLoaded',
        data: conversation,
      });
    }
  }

  private async handleNewConversation(): Promise<void> {
    const newConv = await this.conversationStore.createConversation();
    this.currentConversationId = newConv.id;
    this.sendMessageToWebView({
      type: 'conversationLoaded',
      data: newConv,
    });
  }

  private async handleDeleteConversation(id: string): Promise<void> {
    await this.conversationStore.deleteConversation(id);
    this.sendMessageToWebView({
      type: 'conversationDeleted',
      data: { id },
    });
  }

  private async handleGetQwenSessions(): Promise<void> {
    try {
      console.log('[WebViewProvider] Getting Qwen sessions...');
      const sessions = await this.agentManager.getSessionList();
      console.log('[WebViewProvider] Retrieved sessions:', sessions.length);

      this.sendMessageToWebView({
        type: 'qwenSessionList',
        data: { sessions },
      });
    } catch (error) {
      console.error('[WebViewProvider] Failed to get Qwen sessions:', error);
      this.sendMessageToWebView({
        type: 'error',
        data: { message: `Failed to get sessions: ${error}` },
      });
    }
  }

  private async handleNewQwenSession(): Promise<void> {
    try {
      console.log('[WebViewProvider] Creating new Qwen session...');
      const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
      const workingDir = workspaceFolder?.uri.fsPath || process.cwd();

      await this.agentManager.createNewSession(workingDir);

      // Clear current conversation UI
      this.sendMessageToWebView({
        type: 'conversationCleared',
        data: {},
      });
    } catch (error) {
      console.error('[WebViewProvider] Failed to create new session:', error);
      this.sendMessageToWebView({
        type: 'error',
        data: { message: `Failed to create new session: ${error}` },
      });
    }
  }

  private async handleSwitchQwenSession(sessionId: string): Promise<void> {
    try {
      console.log('[WebViewProvider] Switching to Qwen session:', sessionId);

      // Get session messages from local files
      const messages = await this.agentManager.getSessionMessages(sessionId);
      console.log(
        '[WebViewProvider] Loaded messages from session:',
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
        console.log('[WebViewProvider] Could not get session details:', err);
      }

      // TESTING: Try to load session via ACP first, fallback to creating new session
      const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
      const workingDir = workspaceFolder?.uri.fsPath || process.cwd();

      try {
        console.log('[WebViewProvider] Testing session/load via ACP...');
        const loadResponse =
          await this.agentManager.loadSessionViaAcp(sessionId);
        console.log('[WebViewProvider] session/load succeeded:', loadResponse);

        // If load succeeded, use the loaded session
        this.currentConversationId = sessionId;
        console.log(
          '[WebViewProvider] Set currentConversationId to loaded session:',
          sessionId,
        );
      } catch (_loadError) {
        console.log(
          '[WebViewProvider] session/load not supported, creating new session',
        );

        // Fallback: CLI doesn't support loading old sessions
        // So we create a NEW ACP session for continuation
        try {
          const newAcpSessionId =
            await this.agentManager.createNewSession(workingDir);
          console.log(
            '[WebViewProvider] Created new ACP session for conversation:',
            newAcpSessionId,
          );

          // Use the NEW ACP session ID for sending messages to CLI
          this.currentConversationId = newAcpSessionId;
          console.log(
            '[WebViewProvider] Set currentConversationId (ACP) to:',
            newAcpSessionId,
          );
        } catch (createError) {
          console.error(
            '[WebViewProvider] Failed to create new ACP session:',
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
      this.sendMessageToWebView({
        type: 'qwenSessionSwitched',
        data: { sessionId, messages, session: sessionDetails },
      });
    } catch (error) {
      console.error('[WebViewProvider] Failed to switch session:', error);
      this.sendMessageToWebView({
        type: 'error',
        data: { message: `Failed to switch session: ${error}` },
      });
      vscode.window.showErrorMessage(`Failed to switch session: ${error}`);
    }
  }

  /**
   * Handle cancel prompt request from WebView
   * Cancels the current AI response generation
   */
  private async handleCancelPrompt(): Promise<void> {
    try {
      console.log('[WebViewProvider] Cancel prompt requested');

      if (!this.agentManager.isConnected) {
        console.warn('[WebViewProvider] Agent not connected, cannot cancel');
        return;
      }

      await this.agentManager.cancelCurrentPrompt();

      this.sendMessageToWebView({
        type: 'promptCancelled',
        data: { timestamp: Date.now() },
      });

      console.log('[WebViewProvider] Prompt cancelled successfully');
    } catch (error) {
      console.error('[WebViewProvider] Failed to cancel prompt:', error);
      this.sendMessageToWebView({
        type: 'error',
        data: { message: `Failed to cancel: ${error}` },
      });
    }
  }

  private sendMessageToWebView(message: unknown): void {
    this.panel?.webview.postMessage(message);
  }

  private getWebviewContent(): string {
    const scriptUri = this.panel!.webview.asWebviewUri(
      vscode.Uri.joinPath(this.extensionUri, 'dist', 'webview.js'),
    );

    // Convert extension URI for webview access - this allows frontend to construct resource paths
    const extensionUri = this.panel!.webview.asWebviewUri(this.extensionUri);

    return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src ${this.panel!.webview.cspSource}; script-src ${this.panel!.webview.cspSource}; style-src ${this.panel!.webview.cspSource} 'unsafe-inline';">
  <title>Qwen Code Chat</title>
</head>
<body data-extension-uri="${extensionUri}">
  <div id="root"></div>
  <script src="${scriptUri}"></script>
</body>
</html>`;
  }

  /**
   * Reset agent initialization state
   * Call this when auth cache is cleared to force re-authentication
   */
  resetAgentState(): void {
    console.log('[WebViewProvider] Resetting agent state');
    this.agentInitialized = false;
    // Disconnect existing connection
    this.agentManager.disconnect();
  }

  /**
   * Capture the VS Code Tab that corresponds to our WebviewPanel.
   * We do not pin or lock the editor group, mirroring Claude's approach.
   * Instead, we:
   *  - open beside the active editor
   *  - preserve focus to keep typing in the current file
   *  - keep a Tab reference for reveal/visibility bookkeeping if needed
   */
  private capturePanelTab(): void {
    if (!this.panel) {
      return;
    }

    // Defer slightly so the tab model is updated after create/reveal
    setTimeout(() => {
      const allTabs = vscode.window.tabGroups.all.flatMap((g) => g.tabs);
      const match = allTabs.find((t) => {
        // Type guard for webview tab input
        const input: unknown = (t as { input?: unknown }).input;
        const isWebviewInput = (inp: unknown): inp is { viewType: string } =>
          !!inp && typeof inp === 'object' && 'viewType' in inp;
        const isWebview = isWebviewInput(input);
        const sameViewType = isWebview && input.viewType === 'qwenCode.chat';
        const sameLabel = t.label === this.panel!.title;
        return !!(sameViewType || sameLabel);
      });
      this.panelTab = match ?? null;
    }, 50);
  }

  /**
   * Reveal the WebView panel (optionally preserving focus)
   * We track the tab for bookkeeping, but use panel.reveal for actual reveal
   */
  private revealPanelTab(preserveFocus: boolean = true): void {
    if (this.panel) {
      this.panel.reveal(vscode.ViewColumn.Beside, preserveFocus);
    }
  }

  /**
   * Restore an existing WebView panel (called during VSCode restart)
   * This sets up the panel with all event listeners
   */
  restorePanel(panel: vscode.WebviewPanel): void {
    console.log('[WebViewProvider] Restoring WebView panel');
    this.panel = panel;

    // Set panel icon to Qwen logo
    this.panel.iconPath = vscode.Uri.joinPath(
      this.extensionUri,
      'assets',
      'icon.png',
    );

    // Set webview HTML
    this.panel.webview.html = this.getWebviewContent();

    // Handle messages from WebView
    this.panel.webview.onDidReceiveMessage(
      async (message) => {
        await this.handleWebViewMessage(message);
      },
      null,
      this.disposables,
    );

    // Listen for view state changes (track the tab only)
    this.panel.onDidChangeViewState(
      () => {
        if (this.panel && this.panel.visible) {
          this.capturePanelTab();
        }
      },
      null,
      this.disposables,
    );

    this.panel.onDidDispose(
      () => {
        this.panel = null;
        this.disposables.forEach((d) => d.dispose());
      },
      null,
      this.disposables,
    );

    // Track the tab reference on restore
    this.capturePanelTab();

    console.log('[WebViewProvider] Panel restored successfully');

    // Initialize agent connection if not already done
    if (!this.agentInitialized) {
      console.log(
        '[WebViewProvider] Initializing agent connection after restore...',
      );
      this.initializeAgentConnection().catch((error) => {
        console.error(
          '[WebViewProvider] Failed to initialize agent after restore:',
          error,
        );
      });
    } else {
      console.log(
        '[WebViewProvider] Agent already initialized, loading current session...',
      );
      // Reload current session messages
      this.loadCurrentSessionMessages().catch((error) => {
        console.error(
          '[WebViewProvider] Failed to load session messages after restore:',
          error,
        );
      });
    }
  }

  /**
   * Get the current state for serialization
   * This is used when VSCode restarts to restore the WebView
   */
  getState(): {
    conversationId: string | null;
    agentInitialized: boolean;
  } {
    return {
      conversationId: this.currentConversationId,
      agentInitialized: this.agentInitialized,
    };
  }

  /**
   * Restore state after VSCode restart
   */
  restoreState(state: {
    conversationId: string | null;
    agentInitialized: boolean;
  }): void {
    console.log('[WebViewProvider] Restoring state:', state);
    this.currentConversationId = state.conversationId;
    this.agentInitialized = state.agentInitialized;

    // Reload content after restore
    if (this.panel) {
      this.panel.webview.html = this.getWebviewContent();
    }
  }

  dispose(): void {
    this.panel?.dispose();
    this.agentManager.disconnect();
    this.disposables.forEach((d) => d.dispose());
  }
}
