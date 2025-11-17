/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import * as vscode from 'vscode';
import { AcpConnection } from '../acp/AcpConnection.js';
import type {
  AcpSessionUpdate,
  AcpPermissionRequest,
} from '../shared/acpTypes.js';
import {
  QwenSessionReader,
  type QwenSession,
} from '../services/QwenSessionReader.js';
import type { AuthStateManager } from '../auth/AuthStateManager.js';

export interface ChatMessage {
  role: 'user' | 'assistant';
  content: string;
  timestamp: number;
}

export class QwenAgentManager {
  private connection: AcpConnection;
  private sessionReader: QwenSessionReader;
  private onMessageCallback?: (message: ChatMessage) => void;
  private onStreamChunkCallback?: (chunk: string) => void;
  private onPermissionRequestCallback?: (
    request: AcpPermissionRequest,
  ) => Promise<string>;
  private currentWorkingDir: string = process.cwd();

  constructor() {
    this.connection = new AcpConnection();
    this.sessionReader = new QwenSessionReader();

    // Setup session update handler
    this.connection.onSessionUpdate = (data: AcpSessionUpdate) => {
      this.handleSessionUpdate(data);
    };

    // Setup permission request handler
    this.connection.onPermissionRequest = async (
      data: AcpPermissionRequest,
    ) => {
      if (this.onPermissionRequestCallback) {
        const optionId = await this.onPermissionRequestCallback(data);
        return { optionId };
      }
      return { optionId: 'allow_once' };
    };

    // Setup end turn handler
    this.connection.onEndTurn = () => {
      // Notify UI that response is complete
    };
  }

  async connect(
    workingDir: string,
    authStateManager?: AuthStateManager,
  ): Promise<void> {
    this.currentWorkingDir = workingDir;
    const config = vscode.workspace.getConfiguration('qwenCode');
    const cliPath = config.get<string>('qwen.cliPath', 'qwen');
    const openaiApiKey = config.get<string>('qwen.openaiApiKey', '');
    const openaiBaseUrl = config.get<string>('qwen.openaiBaseUrl', '');
    const model = config.get<string>('qwen.model', '');
    const proxy = config.get<string>('qwen.proxy', '');

    // Build additional CLI arguments
    const extraArgs: string[] = [];
    if (openaiApiKey) {
      extraArgs.push('--openai-api-key', openaiApiKey);
    }
    if (openaiBaseUrl) {
      extraArgs.push('--openai-base-url', openaiBaseUrl);
    }
    if (model) {
      extraArgs.push('--model', model);
    }
    if (proxy) {
      extraArgs.push('--proxy', proxy);
      console.log('[QwenAgentManager] Using proxy:', proxy);
    }

    await this.connection.connect('qwen', cliPath, workingDir, extraArgs);

    // Determine auth method based on configuration
    const authMethod = openaiApiKey ? 'openai' : 'qwen-oauth';

    // Check if we have valid cached authentication
    let needsAuth = true;
    if (authStateManager) {
      const hasValidAuth = await authStateManager.hasValidAuth(
        workingDir,
        authMethod,
      );
      if (hasValidAuth) {
        console.log('[QwenAgentManager] Using cached authentication');
        needsAuth = false;
      }
    }

    // Try to restore existing session or create new one
    let sessionRestored = false;

    // Try to get sessions from local files
    console.log('[QwenAgentManager] Reading local session files...');
    try {
      const sessions = await this.sessionReader.getAllSessions(workingDir);

      if (sessions.length > 0) {
        // Use the most recent session
        console.log(
          '[QwenAgentManager] Found existing sessions:',
          sessions.length,
        );
        const lastSession = sessions[0]; // Already sorted by lastUpdated

        // Try to switch to it (this may fail if not supported)
        try {
          await this.connection.switchSession(lastSession.sessionId);
          console.log(
            '[QwenAgentManager] Restored session:',
            lastSession.sessionId,
          );
          sessionRestored = true;
          // If session restored successfully, we don't need to authenticate
          needsAuth = false;
        } catch (switchError) {
          console.log(
            '[QwenAgentManager] session/switch not supported or failed:',
            switchError instanceof Error
              ? switchError.message
              : String(switchError),
          );
          // Will create new session below
        }
      } else {
        console.log('[QwenAgentManager] No existing sessions found');
      }
    } catch (error) {
      // If reading local sessions fails, log and continue
      const errorMessage =
        error instanceof Error ? error.message : String(error);
      console.log(
        '[QwenAgentManager] Failed to read local sessions:',
        errorMessage,
      );
      // Will create new session below
    }

    // Create new session if we couldn't restore one
    if (!sessionRestored) {
      console.log('[QwenAgentManager] Creating new session...');

      // Authenticate only if needed (not cached or session restore failed)
      if (needsAuth) {
        await this.authenticateWithRetry(authMethod, 3);
        // Save successful auth to cache
        if (authStateManager) {
          await authStateManager.saveAuthState(workingDir, authMethod);
        }
      }

      // Try to create session
      try {
        await this.newSessionWithRetry(workingDir, 3);
        console.log('[QwenAgentManager] New session created successfully');
      } catch (sessionError) {
        // If we used cached auth but session creation failed,
        // the cached auth might be invalid (token expired on server)
        // Clear cache and retry with fresh authentication
        if (!needsAuth && authStateManager) {
          console.log(
            '[QwenAgentManager] Session creation failed with cached auth, clearing cache and re-authenticating...',
          );
          await authStateManager.clearAuthState();

          // Retry with fresh authentication
          await this.authenticateWithRetry(authMethod, 3);
          await authStateManager.saveAuthState(workingDir, authMethod);
          await this.newSessionWithRetry(workingDir, 3);
          console.log(
            '[QwenAgentManager] Successfully authenticated and created session after cache invalidation',
          );
        } else {
          // If we already tried with fresh auth, or no auth manager, just throw
          throw sessionError;
        }
      }
    }
  }

  /**
   * Authenticate with retry logic
   */
  private async authenticateWithRetry(
    authMethod: string,
    maxRetries: number,
  ): Promise<void> {
    for (let attempt = 1; attempt <= maxRetries; attempt++) {
      try {
        console.log(
          `[QwenAgentManager] Authenticating (attempt ${attempt}/${maxRetries})...`,
        );
        await this.connection.authenticate(authMethod);
        console.log('[QwenAgentManager] Authentication successful');
        return;
      } catch (error) {
        const errorMessage =
          error instanceof Error ? error.message : String(error);
        console.error(
          `[QwenAgentManager] Authentication attempt ${attempt} failed:`,
          errorMessage,
        );

        if (attempt === maxRetries) {
          throw new Error(
            `Authentication failed after ${maxRetries} attempts: ${errorMessage}`,
          );
        }

        // Wait before retrying (exponential backoff)
        const delay = Math.min(1000 * Math.pow(2, attempt - 1), 5000);
        console.log(`[QwenAgentManager] Retrying in ${delay}ms...`);
        await new Promise((resolve) => setTimeout(resolve, delay));
      }
    }
  }

  /**
   * Create new session with retry logic
   */
  private async newSessionWithRetry(
    workingDir: string,
    maxRetries: number,
  ): Promise<void> {
    for (let attempt = 1; attempt <= maxRetries; attempt++) {
      try {
        console.log(
          `[QwenAgentManager] Creating session (attempt ${attempt}/${maxRetries})...`,
        );
        await this.connection.newSession(workingDir);
        console.log('[QwenAgentManager] Session created successfully');
        return;
      } catch (error) {
        const errorMessage =
          error instanceof Error ? error.message : String(error);
        console.error(
          `[QwenAgentManager] Session creation attempt ${attempt} failed:`,
          errorMessage,
        );

        if (attempt === maxRetries) {
          throw new Error(
            `Session creation failed after ${maxRetries} attempts: ${errorMessage}`,
          );
        }

        // Wait before retrying
        const delay = Math.min(1000 * Math.pow(2, attempt - 1), 5000);
        console.log(`[QwenAgentManager] Retrying in ${delay}ms...`);
        await new Promise((resolve) => setTimeout(resolve, delay));
      }
    }
  }

  async sendMessage(message: string): Promise<void> {
    await this.connection.sendPrompt(message);
  }

  async getSessionList(): Promise<Array<Record<string, unknown>>> {
    try {
      // Read from local session files instead of ACP protocol
      // Get all sessions from all projects
      const sessions = await this.sessionReader.getAllSessions(undefined, true);
      console.log(
        '[QwenAgentManager] Session list from files (all projects):',
        sessions.length,
      );

      // Transform to UI-friendly format
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

  async getSessionMessages(sessionId: string): Promise<ChatMessage[]> {
    try {
      const session = await this.sessionReader.getSession(
        sessionId,
        this.currentWorkingDir,
      );
      if (!session) {
        return [];
      }

      // Convert Qwen messages to ChatMessage format
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

  async createNewSession(workingDir: string): Promise<void> {
    console.log('[QwenAgentManager] Creating new session...');
    await this.connection.newSession(workingDir);
  }

  async switchToSession(sessionId: string): Promise<void> {
    await this.connection.switchSession(sessionId);
  }

  private handleSessionUpdate(data: AcpSessionUpdate): void {
    const update = data.update;

    if (update.sessionUpdate === 'agent_message_chunk') {
      if (update.content?.text && this.onStreamChunkCallback) {
        this.onStreamChunkCallback(update.content.text);
      }
    } else if (update.sessionUpdate === 'tool_call') {
      // Handle tool call updates
      const toolCall = update as { title?: string; status?: string };
      const title = toolCall.title || 'Tool Call';
      const status = toolCall.status || 'pending';

      if (this.onStreamChunkCallback) {
        this.onStreamChunkCallback(`\n🔧 ${title} [${status}]\n`);
      }
    }
  }

  onMessage(callback: (message: ChatMessage) => void): void {
    this.onMessageCallback = callback;
  }

  onStreamChunk(callback: (chunk: string) => void): void {
    this.onStreamChunkCallback = callback;
  }

  onPermissionRequest(
    callback: (request: AcpPermissionRequest) => Promise<string>,
  ): void {
    this.onPermissionRequestCallback = callback;
  }

  disconnect(): void {
    this.connection.disconnect();
  }

  get isConnected(): boolean {
    return this.connection.isConnected;
  }
}
