/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * Qwen连接处理器
 *
 * 负责Qwen Agent的连接建立、认证和会话创建
 */

import * as vscode from 'vscode';
import type { AcpConnection } from '../acp/acpConnection.js';
import type { QwenSessionReader } from '../services/qwenSessionReader.js';
import type { AuthStateManager } from '../auth/authStateManager.js';

/**
 * Qwen连接处理器类
 * 处理连接、认证和会话初始化
 */
export class QwenConnectionHandler {
  /**
   * 连接到Qwen服务并建立会话
   *
   * @param connection - ACP连接实例
   * @param sessionReader - 会话读取器实例
   * @param workingDir - 工作目录
   * @param authStateManager - 认证状态管理器（可选）
   */
  async connect(
    connection: AcpConnection,
    sessionReader: QwenSessionReader,
    workingDir: string,
    authStateManager?: AuthStateManager,
  ): Promise<void> {
    const connectId = Date.now();
    console.log(`\n========================================`);
    console.log(`[QwenAgentManager] 🚀 CONNECT() CALLED - ID: ${connectId}`);
    console.log(`[QwenAgentManager] Call stack:\n${new Error().stack}`);
    console.log(`========================================\n`);

    const config = vscode.workspace.getConfiguration('qwenCode');
    const cliPath = config.get<string>('qwen.cliPath', 'qwen');
    const openaiApiKey = config.get<string>('qwen.openaiApiKey', '');
    const openaiBaseUrl = config.get<string>('qwen.openaiBaseUrl', '');
    const model = config.get<string>('qwen.model', '');
    const proxy = config.get<string>('qwen.proxy', '');

    // 构建额外的CLI参数
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

    await connection.connect('qwen', cliPath, workingDir, extraArgs);

    // 确定认证方法
    const authMethod = openaiApiKey ? 'openai' : 'qwen-oauth';

    // 检查是否有有效的缓存认证
    if (authStateManager) {
      const hasValidAuth = await authStateManager.hasValidAuth(
        workingDir,
        authMethod,
      );
      if (hasValidAuth) {
        console.log('[QwenAgentManager] Using cached authentication');
      }
    }

    // 尝试恢复现有会话或创建新会话
    let sessionRestored = false;

    // 尝试从本地文件获取会话
    console.log('[QwenAgentManager] Reading local session files...');
    try {
      const sessions = await sessionReader.getAllSessions(workingDir);

      if (sessions.length > 0) {
        console.log(
          '[QwenAgentManager] Found existing sessions:',
          sessions.length,
        );
        const lastSession = sessions[0]; // 已按lastUpdated排序

        try {
          await connection.switchSession(lastSession.sessionId);
          console.log(
            '[QwenAgentManager] Restored session:',
            lastSession.sessionId,
          );
          sessionRestored = true;

          // Save auth state after successful session restore
          if (authStateManager) {
            console.log(
              '[QwenAgentManager] Saving auth state after successful session restore',
            );
            await authStateManager.saveAuthState(workingDir, authMethod);
          }
        } catch (switchError) {
          console.log(
            '[QwenAgentManager] session/switch not supported or failed:',
            switchError instanceof Error
              ? switchError.message
              : String(switchError),
          );
        }
      } else {
        console.log('[QwenAgentManager] No existing sessions found');
      }
    } catch (error) {
      const errorMessage =
        error instanceof Error ? error.message : String(error);
      console.log(
        '[QwenAgentManager] Failed to read local sessions:',
        errorMessage,
      );
    }

    // 如果无法恢复会话则创建新会话
    if (!sessionRestored) {
      console.log('[QwenAgentManager] Creating new session...');

      // 检查是否有有效的缓存认证
      let hasValidAuth = false;
      if (authStateManager) {
        hasValidAuth = await authStateManager.hasValidAuth(
          workingDir,
          authMethod,
        );
      }

      // 只在没有有效缓存认证时进行认证
      if (!hasValidAuth) {
        console.log(
          '[QwenAgentManager] Authenticating before creating session...',
        );
        try {
          await connection.authenticate(authMethod);
          console.log('[QwenAgentManager] Authentication successful');

          // 保存认证状态
          if (authStateManager) {
            console.log(
              '[QwenAgentManager] Saving auth state after successful authentication',
            );
            await authStateManager.saveAuthState(workingDir, authMethod);
          }
        } catch (authError) {
          console.error('[QwenAgentManager] Authentication failed:', authError);
          // 清除可能无效的缓存
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

      try {
        console.log(
          '[QwenAgentManager] Creating new session after authentication...',
        );
        await this.newSessionWithRetry(connection, workingDir, 3);
        console.log('[QwenAgentManager] New session created successfully');

        // 确保认证状态已保存（防止重复认证）
        if (authStateManager && !hasValidAuth) {
          console.log(
            '[QwenAgentManager] Saving auth state after successful session creation',
          );
          await authStateManager.saveAuthState(workingDir, authMethod);
        }
      } catch (sessionError) {
        console.log(`\n⚠️ [SESSION FAILED] newSessionWithRetry threw error\n`);
        console.log(`[QwenAgentManager] Error details:`, sessionError);

        // 清除缓存
        if (authStateManager) {
          console.log('[QwenAgentManager] Clearing auth cache due to failure');
          await authStateManager.clearAuthState();
        }

        throw sessionError;
      }
    }

    console.log(`\n========================================`);
    console.log(`[QwenAgentManager] ✅ CONNECT() COMPLETED SUCCESSFULLY`);
    console.log(`========================================\n`);
  }

  /**
   * 创建新会话（带重试）
   *
   * @param connection - ACP连接实例
   * @param workingDir - 工作目录
   * @param maxRetries - 最大重试次数
   */
  private async newSessionWithRetry(
    connection: AcpConnection,
    workingDir: string,
    maxRetries: number,
  ): Promise<void> {
    for (let attempt = 1; attempt <= maxRetries; attempt++) {
      try {
        console.log(
          `[QwenAgentManager] Creating session (attempt ${attempt}/${maxRetries})...`,
        );
        await connection.newSession(workingDir);
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

        const delay = Math.min(1000 * Math.pow(2, attempt - 1), 5000);
        console.log(`[QwenAgentManager] Retrying in ${delay}ms...`);
        await new Promise((resolve) => setTimeout(resolve, delay));
      }
    }
  }
}
