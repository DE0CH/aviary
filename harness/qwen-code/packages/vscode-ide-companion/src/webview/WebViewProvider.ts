/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import * as vscode from 'vscode';
import { QwenAgentManager } from '../agents/qwenAgentManager.js';
import { ConversationStore } from '../storage/conversationStore.js';
import type { AcpPermissionRequest } from '../shared/acpTypes.js';
import { CliDetector } from '../utils/cliDetector.js';
import { AuthStateManager } from '../auth/authStateManager.js';
import { PanelManager } from './PanelManager.js';
import { MessageHandler } from './MessageHandler.js';
import { WebViewContent } from './WebViewContent.js';
import { CliInstaller } from '../utils/CliInstaller.js';
import { getFileName } from '../utils/webviewUtils.js';

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
    authStateManager?: AuthStateManager, // 可选的全局AuthStateManager实例
  ) {
    this.agentManager = new QwenAgentManager();
    this.conversationStore = new ConversationStore(context);
    // 如果提供了全局的authStateManager，则使用它，否则创建新的实例
    this.authStateManager = authStateManager || new AuthStateManager(context);
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

    // Set login handler for /login command - direct force re-login
    this.messageHandler.setLoginHandler(async () => {
      await this.forceReLogin();
    });

    // Setup agent callbacks
    this.agentManager.onStreamChunk((chunk: string) => {
      // Ignore stream chunks from background /chat save commands
      if (this.messageHandler.getIsSavingCheckpoint()) {
        console.log(
          '[WebViewProvider] Ignoring stream chunk from /chat save command',
        );
        return;
      }

      this.messageHandler.appendStreamContent(chunk);
      this.sendMessageToWebView({
        type: 'streamChunk',
        data: { chunk },
      });
    });

    // Setup thought chunk handler
    this.agentManager.onThoughtChunk((chunk: string) => {
      // Ignore thought chunks from background /chat save commands
      if (this.messageHandler.getIsSavingCheckpoint()) {
        console.log(
          '[WebViewProvider] Ignoring thought chunk from /chat save command',
        );
        return;
      }

      this.messageHandler.appendStreamContent(chunk);
      this.sendMessageToWebView({
        type: 'thoughtChunk',
        data: { chunk },
      });
    });

    // Note: Tool call updates are handled in handleSessionUpdate within QwenAgentManager
    // and sent via onStreamChunk callback
    this.agentManager.onToolCall((update) => {
      // Ignore tool calls from background /chat save commands
      if (this.messageHandler.getIsSavingCheckpoint()) {
        console.log(
          '[WebViewProvider] Ignoring tool call from /chat save command',
        );
        return;
      }

      // Cast update to access sessionUpdate property
      const updateData = update as unknown as Record<string, unknown>;

      // Determine message type from sessionUpdate field
      // If sessionUpdate is missing, infer from content:
      // - If has kind/title/rawInput, it's likely initial tool_call
      // - If only has status/content updates, it's tool_call_update
      let messageType = updateData.sessionUpdate as string | undefined;
      if (!messageType) {
        // Infer type: if has kind or title, assume initial call; otherwise update
        if (updateData.kind || updateData.title || updateData.rawInput) {
          messageType = 'tool_call';
        } else {
          messageType = 'tool_call_update';
        }
      }

      this.sendMessageToWebView({
        type: 'toolCall',
        data: {
          type: messageType,
          ...updateData,
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

    // Set up state serialization
    newPanel.onDidChangeViewState(() => {
      console.log(
        '[WebViewProvider] Panel view state changed, triggering serialization check',
      );
    });

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

    // Track last known editor state (to preserve when switching to webview)
    let _lastEditorState: {
      fileName: string | null;
      filePath: string | null;
      selection: {
        startLine: number;
        endLine: number;
      } | null;
    } | null = null;

    // Listen for active editor changes and notify WebView
    const editorChangeDisposable = vscode.window.onDidChangeActiveTextEditor(
      (editor) => {
        // If switching to a non-text editor (like webview), keep the last state
        if (!editor) {
          // Don't update - keep previous state
          return;
        }

        const filePath = editor.document.uri.fsPath || null;
        const fileName = filePath ? getFileName(filePath) : null;

        // Get selection info if there is any selected text
        let selectionInfo = null;
        if (editor && !editor.selection.isEmpty) {
          const selection = editor.selection;
          selectionInfo = {
            startLine: selection.start.line + 1,
            endLine: selection.end.line + 1,
          };
        }

        // Update last known state
        _lastEditorState = { fileName, filePath, selection: selectionInfo };

        this.sendMessageToWebView({
          type: 'activeEditorChanged',
          data: { fileName, filePath, selection: selectionInfo },
        });
      },
    );
    this.disposables.push(editorChangeDisposable);

    // Listen for text selection changes
    const selectionChangeDisposable =
      vscode.window.onDidChangeTextEditorSelection((event) => {
        const editor = event.textEditor;
        if (editor === vscode.window.activeTextEditor) {
          const filePath = editor.document.uri.fsPath || null;
          const fileName = filePath ? getFileName(filePath) : null;

          // Get selection info if there is any selected text
          let selectionInfo = null;
          if (!event.selections[0].isEmpty) {
            const selection = event.selections[0];
            selectionInfo = {
              startLine: selection.start.line + 1,
              endLine: selection.end.line + 1,
            };
          }

          // Update last known state
          _lastEditorState = { fileName, filePath, selection: selectionInfo };

          this.sendMessageToWebView({
            type: 'activeEditorChanged',
            data: { fileName, filePath, selection: selectionInfo },
          });
        }
      });
    this.disposables.push(selectionChangeDisposable);

    // Send initial active editor state to WebView
    const initialEditor = vscode.window.activeTextEditor;
    if (initialEditor) {
      const filePath = initialEditor.document.uri.fsPath || null;
      const fileName = filePath ? getFileName(filePath) : null;

      let selectionInfo = null;
      if (!initialEditor.selection.isEmpty) {
        const selection = initialEditor.selection;
        selectionInfo = {
          startLine: selection.start.line + 1,
          endLine: selection.end.line + 1,
        };
      }

      this.sendMessageToWebView({
        type: 'activeEditorChanged',
        data: { fileName, filePath, selection: selectionInfo },
      });
    }

    // Smart login restore: Check if we have valid cached auth and restore connection if available
    const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
    const workingDir = workspaceFolder?.uri.fsPath || process.cwd();
    const config = vscode.workspace.getConfiguration('qwenCode');
    const openaiApiKey = config.get<string>('qwen.openaiApiKey', '');
    const authMethod = openaiApiKey ? 'openai' : 'qwen-oauth';

    // Check if we have valid cached authentication
    let hasValidAuth = false;
    if (this.authStateManager) {
      hasValidAuth = await this.authStateManager.hasValidAuth(
        workingDir,
        authMethod,
      );
      console.log(
        '[WebViewProvider] Has valid cached auth on show:',
        hasValidAuth,
      );
    }

    if (hasValidAuth && !this.agentInitialized) {
      console.log(
        '[WebViewProvider] Found valid cached auth, attempting to restore connection...',
      );
      try {
        await this.initializeAgentConnection();
        console.log('[WebViewProvider] Connection restored successfully');
      } catch (error) {
        console.error('[WebViewProvider] Failed to restore connection:', error);
        // Fall back to empty conversation if restore fails
        await this.initializeEmptyConversation();
      }
    } else if (this.agentInitialized) {
      console.log(
        '[WebViewProvider] Agent already initialized, reusing existing connection',
      );
      // Reload current session messages
      await this.loadCurrentSessionMessages();
    } else {
      console.log(
        '[WebViewProvider] No valid cached auth or agent already initialized, showing empty conversation',
      );
      // Just initialize empty conversation for the UI
      await this.initializeEmptyConversation();
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
          console.log(
            '[WebViewProvider] Using authStateManager:',
            !!this.authStateManager,
          );
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
   * Force re-login by clearing auth cache and reconnecting
   * Called when user explicitly uses /login command
   */
  async forceReLogin(): Promise<void> {
    console.log('[WebViewProvider] Force re-login requested');
    console.log(
      '[WebViewProvider] Current authStateManager:',
      !!this.authStateManager,
    );

    // Clear existing auth cache
    if (this.authStateManager) {
      await this.authStateManager.clearAuthState();
      console.log('[WebViewProvider] Auth cache cleared');
    } else {
      console.log('[WebViewProvider] No authStateManager to clear');
    }

    // Disconnect existing connection if any
    if (this.agentInitialized) {
      try {
        this.agentManager.disconnect();
        console.log('[WebViewProvider] Existing connection disconnected');
      } catch (error) {
        console.log('[WebViewProvider] Error disconnecting:', error);
      }
      this.agentInitialized = false;
    }

    // Wait a moment for cleanup to complete
    await new Promise((resolve) => setTimeout(resolve, 500));

    // Reinitialize connection (will trigger fresh authentication)
    try {
      await this.initializeAgentConnection();
      console.log('[WebViewProvider] Force re-login completed successfully');

      // Send success notification to WebView
      this.sendMessageToWebView({
        type: 'loginSuccess',
        data: { message: 'Successfully logged in!' },
      });
    } catch (error) {
      console.error('[WebViewProvider] Force re-login failed:', error);

      // Send error notification to WebView
      this.sendMessageToWebView({
        type: 'loginError',
        data: { message: `Login failed: ${error}` },
      });

      throw error;
    }
  }

  /**
   * Refresh connection without clearing auth cache
   * Called when restoring WebView after VSCode restart
   */
  async refreshConnection(): Promise<void> {
    console.log('[WebViewProvider] Refresh connection requested');

    // Disconnect existing connection if any
    if (this.agentInitialized) {
      try {
        this.agentManager.disconnect();
        console.log('[WebViewProvider] Existing connection disconnected');
      } catch (error) {
        console.log('[WebViewProvider] Error disconnecting:', error);
      }
      this.agentInitialized = false;
    }

    // Wait a moment for cleanup to complete
    await new Promise((resolve) => setTimeout(resolve, 500));

    // Reinitialize connection (will use cached auth if available)
    try {
      await this.initializeAgentConnection();
      console.log(
        '[WebViewProvider] Connection refresh completed successfully',
      );
    } catch (error) {
      console.error('[WebViewProvider] Connection refresh failed:', error);
      throw error;
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
  async restorePanel(panel: vscode.WebviewPanel): Promise<void> {
    console.log('[WebViewProvider] Restoring WebView panel');
    console.log(
      '[WebViewProvider] Current authStateManager in restore:',
      !!this.authStateManager,
    );
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

    // Track last known editor state (to preserve when switching to webview)
    let _lastEditorState: {
      fileName: string | null;
      filePath: string | null;
      selection: {
        startLine: number;
        endLine: number;
      } | null;
    } | null = null;

    // Listen for active editor changes and notify WebView
    const editorChangeDisposable = vscode.window.onDidChangeActiveTextEditor(
      (editor) => {
        // If switching to a non-text editor (like webview), keep the last state
        if (!editor) {
          // Don't update - keep previous state
          return;
        }

        const filePath = editor.document.uri.fsPath || null;
        const fileName = filePath ? getFileName(filePath) : null;

        // Get selection info if there is any selected text
        let selectionInfo = null;
        if (editor && !editor.selection.isEmpty) {
          const selection = editor.selection;
          selectionInfo = {
            startLine: selection.start.line + 1,
            endLine: selection.end.line + 1,
          };
        }

        // Update last known state
        _lastEditorState = { fileName, filePath, selection: selectionInfo };

        this.sendMessageToWebView({
          type: 'activeEditorChanged',
          data: { fileName, filePath, selection: selectionInfo },
        });
      },
    );
    this.disposables.push(editorChangeDisposable);

    // Send initial active editor state to WebView
    const initialEditor = vscode.window.activeTextEditor;
    if (initialEditor) {
      const filePath = initialEditor.document.uri.fsPath || null;
      const fileName = filePath ? getFileName(filePath) : null;

      let selectionInfo = null;
      if (!initialEditor.selection.isEmpty) {
        const selection = initialEditor.selection;
        selectionInfo = {
          startLine: selection.start.line + 1,
          endLine: selection.end.line + 1,
        };
      }

      this.sendMessageToWebView({
        type: 'activeEditorChanged',
        data: { fileName, filePath, selection: selectionInfo },
      });
    }

    // Capture the tab reference on restore
    this.panelManager.captureTab();

    console.log('[WebViewProvider] Panel restored successfully');

    // Smart login restore: Check if we have valid cached auth and restore connection if available
    const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
    const workingDir = workspaceFolder?.uri.fsPath || process.cwd();
    const config = vscode.workspace.getConfiguration('qwenCode');
    const openaiApiKey = config.get<string>('qwen.openaiApiKey', '');
    const authMethod = openaiApiKey ? 'openai' : 'qwen-oauth';

    // Check if we have valid cached authentication
    let hasValidAuth = false;
    if (this.authStateManager) {
      hasValidAuth = await this.authStateManager.hasValidAuth(
        workingDir,
        authMethod,
      );
      console.log(
        '[WebViewProvider] Has valid cached auth on restore:',
        hasValidAuth,
      );
    }

    if (hasValidAuth && !this.agentInitialized) {
      console.log(
        '[WebViewProvider] Found valid cached auth, attempting to restore connection...',
      );
      try {
        await this.initializeAgentConnection();
        console.log('[WebViewProvider] Connection restored successfully');
      } catch (error) {
        console.error('[WebViewProvider] Failed to restore connection:', error);
        // Fall back to empty conversation if restore fails
        await this.initializeEmptyConversation();
      }
    } else if (this.agentInitialized) {
      console.log(
        '[WebViewProvider] Agent already initialized, refreshing connection...',
      );
      try {
        await this.refreshConnection();
        console.log('[WebViewProvider] Connection refreshed successfully');
      } catch (error) {
        console.error('[WebViewProvider] Failed to refresh connection:', error);
        // Fall back to empty conversation if refresh fails
        this.agentInitialized = false;
        await this.initializeEmptyConversation();
      }
    } else {
      console.log(
        '[WebViewProvider] No valid cached auth or agent already initialized, showing empty conversation',
      );
      // Just initialize empty conversation for the UI
      await this.initializeEmptyConversation();
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
    console.log('[WebViewProvider] Getting state for serialization');
    console.log(
      '[WebViewProvider] Current conversationId:',
      this.messageHandler.getCurrentConversationId(),
    );
    console.log(
      '[WebViewProvider] Current agentInitialized:',
      this.agentInitialized,
    );
    const state = {
      conversationId: this.messageHandler.getCurrentConversationId(),
      agentInitialized: this.agentInitialized,
    };
    console.log('[WebViewProvider] Returning state:', state);
    return state;
  }

  /**
   * Get the current panel
   */
  getPanel(): vscode.WebviewPanel | null {
    return this.panelManager.getPanel();
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
    console.log(
      '[WebViewProvider] State restored. agentInitialized:',
      this.agentInitialized,
    );

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
