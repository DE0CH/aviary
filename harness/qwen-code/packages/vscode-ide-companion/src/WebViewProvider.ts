/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import * as vscode from 'vscode';
import { QwenAgentManager } from './agents/qwenAgentManager.js';
import { ConversationStore } from './storage/conversationStore.js';
import type { AcpPermissionRequest } from './shared/acpTypes.js';
import { CliDetector } from './utils/cliDetector.js';
import { AuthStateManager } from './auth/authStateManager.js';
import { PanelManager } from './webview/PanelManager.js';
import { MessageHandler } from './webview/MessageHandler.js';
import { WebViewContent } from './webview/WebViewContent.js';
import { CliInstaller } from './webview/CliInstaller.js';
import { getFileName } from './utils/webviewUtils.js';

export class WebViewProvider {
  private panelManager: PanelManager;
  private messageHandler: MessageHandler;
  private agentManager: QwenAgentManager;
  private conversationStore: ConversationStore;
  private authStateManager: AuthStateManager;
  private disposables: vscode.Disposable[] = [];
  private agentInitialized = false; // Track if agent has been initialized

  constructor(
    context: vscode.ExtensionContext,
    private extensionUri: vscode.Uri,
  ) {
    this.agentManager = new QwenAgentManager();
    this.conversationStore = new ConversationStore(context);
    this.authStateManager = new AuthStateManager(context);
    this.panelManager = new PanelManager(extensionUri, () => {
      // Panel dispose callback
      this.disposables.forEach((d) => d.dispose());
    });
    this.messageHandler = new MessageHandler(
      this.agentManager,
      this.conversationStore,
      null,
      (message) => this.sendMessageToWebView(message),
    );

    // Set login handler for /login command
    this.messageHandler.setLoginHandler(async () => {
      await this.initializeAgentConnection();
    });

    // Setup agent callbacks
    this.agentManager.onStreamChunk((chunk: string) => {
      this.messageHandler.appendStreamContent(chunk);
      this.sendMessageToWebView({
        type: 'streamChunk',
        data: { chunk },
      });
    });

    // Setup thought chunk handler
    this.agentManager.onThoughtChunk((chunk: string) => {
      this.messageHandler.appendStreamContent(chunk);
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
          // Store handler in message handler
          this.messageHandler.setPermissionHandler(handler);
        });
      },
    );
  }

  async show(): Promise<void> {
    const panel = this.panelManager.getPanel();

    if (panel) {
      // Reveal the existing panel
      this.panelManager.revealPanel(true);
      this.panelManager.captureTab();
      return;
    }

    // Create new panel
    const isNewPanel = await this.panelManager.createPanel();

    if (!isNewPanel) {
      return; // Failed to create panel
    }

    const newPanel = this.panelManager.getPanel();
    if (!newPanel) {
      return;
    }

    // Capture the Tab that corresponds to our WebviewPanel
    this.panelManager.captureTab();

    // Auto-lock editor group when opened in new column
    await this.panelManager.autoLockEditorGroup();

    newPanel.webview.html = WebViewContent.generate(
      newPanel,
      this.extensionUri,
    );

    // Handle messages from WebView
    newPanel.webview.onDidReceiveMessage(
      async (message: { type: string; data?: unknown }) => {
        await this.messageHandler.route(message);
      },
      null,
      this.disposables,
    );

    // Listen for view state changes (no pin/lock; just keep tab reference fresh)
    this.panelManager.registerViewStateChangeHandler(this.disposables);

    // Register panel dispose handler
    this.panelManager.registerDisposeHandler(this.disposables);

    // Listen for active editor changes and notify WebView
    const editorChangeDisposable = vscode.window.onDidChangeActiveTextEditor(
      (editor) => {
        const fileName = editor?.document.uri.fsPath
          ? getFileName(editor.document.uri.fsPath)
          : null;
        this.sendMessageToWebView({
          type: 'activeEditorChanged',
          data: { fileName },
        });
      },
    );
    this.disposables.push(editorChangeDisposable);

    // Don't auto-login; user must use /login command
    // Just initialize empty conversation for the UI
    if (!this.agentInitialized) {
      console.log(
        '[WebViewProvider] Agent not initialized, waiting for /login command',
      );
      await this.initializeEmptyConversation();
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
   * Can be called from show() or via /login command
   */
  async initializeAgentConnection(): Promise<void> {
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
        await CliInstaller.promptInstallation();

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

  /**
   * Load messages from current Qwen session
   * Creates a new ACP session for immediate message sending
   */
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

  /**
   * Initialize an empty conversation
   * Creates a new conversation and notifies WebView
   */
  private async initializeEmptyConversation(): Promise<void> {
    try {
      console.log('[WebViewProvider] Initializing empty conversation');
      const newConv = await this.conversationStore.createConversation();
      this.messageHandler.setCurrentConversationId(newConv.id);
      this.sendMessageToWebView({
        type: 'conversationLoaded',
        data: newConv,
      });
      console.log(
        '[WebViewProvider] Empty conversation initialized:',
        this.messageHandler.getCurrentConversationId(),
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

  /**
   * Send message to WebView
   */
  private sendMessageToWebView(message: unknown): void {
    const panel = this.panelManager.getPanel();
    panel?.webview.postMessage(message);
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
   * Restore an existing WebView panel (called during VSCode restart)
   * This sets up the panel with all event listeners
   */
  restorePanel(panel: vscode.WebviewPanel): void {
    console.log('[WebViewProvider] Restoring WebView panel');
    this.panelManager.setPanel(panel);

    panel.webview.html = WebViewContent.generate(panel, this.extensionUri);

    // Handle messages from WebView
    panel.webview.onDidReceiveMessage(
      async (message: { type: string; data?: unknown }) => {
        await this.messageHandler.route(message);
      },
      null,
      this.disposables,
    );

    // Register view state change handler
    this.panelManager.registerViewStateChangeHandler(this.disposables);

    // Register dispose handler
    this.panelManager.registerDisposeHandler(this.disposables);

    // Listen for active editor changes and notify WebView
    const editorChangeDisposable = vscode.window.onDidChangeActiveTextEditor(
      (editor) => {
        const fileName = editor?.document.uri.fsPath
          ? getFileName(editor.document.uri.fsPath)
          : null;
        this.sendMessageToWebView({
          type: 'activeEditorChanged',
          data: { fileName },
        });
      },
    );
    this.disposables.push(editorChangeDisposable);

    // Capture the tab reference on restore
    this.panelManager.captureTab();

    console.log('[WebViewProvider] Panel restored successfully');

    // Don't auto-login on restore; user must use /login command
    // Just initialize empty conversation for the UI
    if (!this.agentInitialized) {
      console.log(
        '[WebViewProvider] Agent not initialized after restore, waiting for /login command',
      );
      this.initializeEmptyConversation().catch((error) => {
        console.error(
          '[WebViewProvider] Failed to initialize empty conversation after restore:',
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
      conversationId: this.messageHandler.getCurrentConversationId(),
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
    this.messageHandler.setCurrentConversationId(state.conversationId);
    this.agentInitialized = state.agentInitialized;

    // Reload content after restore
    const panel = this.panelManager.getPanel();
    if (panel) {
      panel.webview.html = WebViewContent.generate(panel, this.extensionUri);
    }
  }

  /**
   * Create a new session in the current panel
   * This is called when the user clicks the "New Session" button
   */
  async createNewSession(): Promise<void> {
    console.log('[WebViewProvider] Creating new session in current panel');
    try {
      const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
      const workingDir = workspaceFolder?.uri.fsPath || process.cwd();

      // Create new Qwen session via agent manager
      await this.agentManager.createNewSession(workingDir);

      // Clear current conversation UI
      this.sendMessageToWebView({
        type: 'conversationCleared',
        data: {},
      });

      console.log('[WebViewProvider] New session created successfully');
    } catch (error) {
      console.error('[WebViewProvider] Failed to create new session:', error);
      vscode.window.showErrorMessage(`Failed to create new session: ${error}`);
    }
  }

  /**
   * Dispose the WebView provider and clean up resources
   */
  dispose(): void {
    this.panelManager.dispose();
    this.agentManager.disconnect();
    this.disposables.forEach((d) => d.dispose());
  }
}
