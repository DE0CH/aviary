/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * Qwen Connection Handler
 *
 * Handles Qwen Agent connection establishment, authentication, and session creation
 */

import type { AcpConnection } from './acpConnection.js';

export interface QwenConnectionResult {
  sessionCreated: boolean;
  requiresAuth: boolean;
}

/**
 * Qwen Connection Handler class
 * Handles connection, authentication, and session initialization
 */
export class QwenConnectionHandler {
  /**
   * Connect to Qwen service and establish session
   *
   * @param connection - ACP connection instance
   * @param workingDir - Working directory
   * @param cliEntryPath - Path to bundled CLI entrypoint (cli.js)
   */
  async connect(
    connection: AcpConnection,
    workingDir: string,
    cliEntryPath: string,
  ): Promise<QwenConnectionResult> {
    const connectId = Date.now();
    console.log(`[QwenAgentManager] 🚀 CONNECT() CALLED - ID: ${connectId}`);
    const sessionCreated = false;
    const requiresAuth = false;

    // Build extra CLI arguments (only essential parameters)
    const extraArgs: string[] = [];

    await connection.connect(cliEntryPath, workingDir, extraArgs);

    // Note: Session creation is now handled by the caller (QwenAgentManager)
    // This prevents automatic session creation on every connection which was
    // causing unwanted authentication prompts

    console.log(`\n========================================`);
    console.log(`[QwenAgentManager] ✅ CONNECT() COMPLETED SUCCESSFULLY`);
    console.log(`========================================\n`);
    return { sessionCreated, requiresAuth };
  }
}
