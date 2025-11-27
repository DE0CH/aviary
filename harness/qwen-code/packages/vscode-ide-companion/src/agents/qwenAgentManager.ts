/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */
import * as vscode from 'vscode';
import { AcpConnection } from '../acp/acpConnection.js';
import type {
  AcpSessionUpdate,
  AcpPermissionRequest,
} from '../constants/acpTypes.js';
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
 * Qwen Agent Manager
 *
 * Coordinates various modules and provides unified interface
 */
export class QwenAgentManager {
  private connection: AcpConnection;
  private sessionReader: QwenSessionReader;
  private sessionManager: QwenSessionManager;
  private connectionHandler: QwenConnectionHandler;
  private sessionUpdateHandler: QwenSessionUpdateHandler;
  private currentWorkingDir: string = process.cwd();

  // Callback storage
  private callbacks: QwenAgentCallbacks = {};

  constructor() {
    this.connection = new AcpConnection();
    this.sessionReader = new QwenSessionReader();
    this.sessionManager = new QwenSessionManager();
    this.connectionHandler = new QwenConnectionHandler();
    this.sessionUpdateHandler = new QwenSessionUpdateHandler({});

    // Set ACP connection callbacks
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
      // Notify UI response complete
    };
  }

  /**
   * Connect to Qwen service
   *
   * @param workingDir - Working directory
   * @param authStateManager - Authentication state manager (optional)
   * @param cliPath - CLI path (optional, if provided will override the path in configuration)
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
   * Send message
   *
   * @param message - Message content
   */
  async sendMessage(message: string): Promise<void> {
    await this.connection.sendPrompt(message);
  }

  /**
   * Validate if current session is still active
   * This is a lightweight check to verify session validity
   *
   * @returns True if session is valid, false otherwise
   */
  async validateCurrentSession(): Promise<boolean> {
    try {
      // If we don't have a current session, it's definitely not valid
      if (!this.connection.currentSessionId) {
        return false;
      }

      // Try to get session list to verify our session still exists
      const sessions = await this.getSessionList();
      const currentSessionId = this.connection.currentSessionId;

      // Check if our current session exists in the session list
      const sessionExists = sessions.some(
        (session: Record<string, unknown>) =>
          session.id === currentSessionId ||
          session.sessionId === currentSessionId,
      );

      return sessionExists;
    } catch (error) {
      console.warn('[QwenAgentManager] Session validation failed:', error);
      // If we can't validate, assume session is invalid
      return false;
    }
  }

  /**
   * Get session list with version-aware strategy
   * First tries ACP method if CLI version supports it, falls back to file system method
   *
   * @returns Session list
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
   * Get session messages (read from disk)
   *
   * @param sessionId - Session ID
   * @returns Message list
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
   * Save session via /chat save command
   * Since CLI doesn't support session/save ACP method, we send /chat save command directly
   *
   * @param sessionId - Session ID
   * @param tag - Save tag
   * @returns Save response
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
   * Save session via ACP session/save method (deprecated, CLI doesn't support)
   *
   * @deprecated Use saveSessionViaCommand instead
   * @param sessionId - Session ID
   * @param tag - Save tag
   * @returns Save response
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
   * Save session via /chat save command (CLI way)
   * Calls CLI's native save function to ensure complete content is saved
   *
   * @param tag - Checkpoint tag
   * @returns Save result
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
   * Save session as checkpoint (using CLI format)
   * Saves to ~/.qwen/tmp/{projectHash}/checkpoint-{tag}.json
   * Saves two copies with sessionId and conversationId to ensure recovery via either ID
   *
   * @param messages - Current session messages
   * @param conversationId - Conversation ID (from VSCode extension)
   * @returns Save result
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
   * Save session directly to file system (without relying on ACP)
   *
   * @param messages - Current session messages
   * @param sessionName - Session name
   * @returns Save result
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
   * Try to load session via ACP session/load method
   * This is a test method to verify if CLI supports session/load
   *
   * @param sessionId - Session ID
   * @returns Load response or error
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
   * Load session directly from file system (without relying on ACP)
   *
   * @param sessionId - Session ID
   * @returns Loaded session messages or null
   */
  async loadSessionDirect(sessionId: string): Promise<ChatMessage[] | null> {
    try {
      console.log('[QwenAgentManager] Loading session directly:', sessionId);

      // Load session
      const session = await this.sessionManager.loadSession(
        sessionId,
        this.currentWorkingDir,
      );

      if (!session) {
        console.log('[QwenAgentManager] Session not found:', sessionId);
        return null;
      }

      // Convert message format
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
   * Create new session
   *
   * Note: Authentication should be done in connect() method, only create session here
   *
   * @param workingDir - Working directory
   * @returns Newly created session ID
   */
  async createNewSession(
    workingDir: string,
    authStateManager?: AuthStateManager,
  ): Promise<string | null> {
    console.log('[QwenAgentManager] Creating new session...');

    // Check if we have valid cached authentication
    let hasValidAuth = false;
    const config = vscode.workspace.getConfiguration('qwenCode');
    const openaiApiKey = config.get<string>('qwen.openaiApiKey', '');
    const authMethod = openaiApiKey ? 'openai' : 'qwen-oauth';

    if (authStateManager) {
      hasValidAuth = await authStateManager.hasValidAuth(
        workingDir,
        authMethod,
      );
      console.log(
        '[QwenAgentManager] Has valid cached auth for new session:',
        hasValidAuth,
      );
    }

    // Only authenticate if we don't have valid cached auth
    if (!hasValidAuth) {
      console.log(
        '[QwenAgentManager] Authenticating before creating session...',
      );
      try {
        await this.connection.authenticate(authMethod);
        console.log('[QwenAgentManager] Authentication successful');

        // Save auth state
        if (authStateManager) {
          console.log(
            '[QwenAgentManager] Saving auth state after successful authentication',
          );
          await authStateManager.saveAuthState(workingDir, authMethod);
        }
      } catch (authError) {
        console.error('[QwenAgentManager] Authentication failed:', authError);
        // Clear potentially invalid cache
        if (authStateManager) {
          console.log(
            '[QwenAgentManager] Clearing auth cache due to authentication failure',
          );
          await authStateManager.clearAuthState();
        }
        throw authError;
      }
    } else {
      console.log(
        '[QwenAgentManager] Skipping authentication - using valid cached auth',
      );
    }

    await this.connection.newSession(workingDir);
    const newSessionId = this.connection.currentSessionId;
    console.log(
      '[QwenAgentManager] New session created with ID:',
      newSessionId,
    );
    return newSessionId;
  }

  /**
   * Switch to specified session
   *
   * @param sessionId - Session ID
   */
  async switchToSession(sessionId: string): Promise<void> {
    await this.connection.switchSession(sessionId);
  }

  /**
   * Cancel current prompt
   */
  async cancelCurrentPrompt(): Promise<void> {
    console.log('[QwenAgentManager] Cancelling current prompt');
    await this.connection.cancelSession();
  }

  /**
   * Register message callback
   *
   * @param callback - Message callback function
   */
  onMessage(callback: (message: ChatMessage) => void): void {
    this.callbacks.onMessage = callback;
    this.sessionUpdateHandler.updateCallbacks(this.callbacks);
  }

  /**
   * Register stream chunk callback
   *
   * @param callback - Stream chunk callback function
   */
  onStreamChunk(callback: (chunk: string) => void): void {
    this.callbacks.onStreamChunk = callback;
    this.sessionUpdateHandler.updateCallbacks(this.callbacks);
  }

  /**
   * Register thought chunk callback
   *
   * @param callback - Thought chunk callback function
   */
  onThoughtChunk(callback: (chunk: string) => void): void {
    this.callbacks.onThoughtChunk = callback;
    this.sessionUpdateHandler.updateCallbacks(this.callbacks);
  }

  /**
   * Register tool call callback
   *
   * @param callback - Tool call callback function
   */
  onToolCall(callback: (update: ToolCallUpdateData) => void): void {
    this.callbacks.onToolCall = callback;
    this.sessionUpdateHandler.updateCallbacks(this.callbacks);
  }

  /**
   * Register plan callback
   *
   * @param callback - Plan callback function
   */
  onPlan(callback: (entries: PlanEntry[]) => void): void {
    this.callbacks.onPlan = callback;
    this.sessionUpdateHandler.updateCallbacks(this.callbacks);
  }

  /**
   * Register permission request callback
   *
   * @param callback - Permission request callback function
   */
  onPermissionRequest(
    callback: (request: AcpPermissionRequest) => Promise<string>,
  ): void {
    this.callbacks.onPermissionRequest = callback;
    this.sessionUpdateHandler.updateCallbacks(this.callbacks);
  }

  /**
   * Disconnect
   */
  disconnect(): void {
    this.connection.disconnect();
  }

  /**
   * Check if connected
   */
  get isConnected(): boolean {
    return this.connection.isConnected;
  }

  /**
   * Get current session ID
   */
  get currentSessionId(): string | null {
    return this.connection.currentSessionId;
  }
}
