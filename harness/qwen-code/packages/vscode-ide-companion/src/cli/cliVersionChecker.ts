/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import * as vscode from 'vscode';
import { CliContextManager } from './cliContextManager.js';
import { CliVersionManager } from './cliVersionManager.js';
import { MIN_CLI_VERSION_FOR_SESSION_METHODS } from './cliVersionManager.js';
import type { CliVersionInfo } from './cliVersionManager.js';

/**
 * Check CLI version and show warning if below minimum requirement
 *
 * @returns Version information
 */
export async function checkCliVersionAndWarn(): Promise<void> {
  try {
    const cliContextManager = CliContextManager.getInstance();
    const versionInfo =
      await CliVersionManager.getInstance().detectCliVersion(true);
    cliContextManager.setCurrentVersionInfo(versionInfo);

    if (!versionInfo.isSupported) {
      vscode.window.showWarningMessage(
        `Qwen Code CLI version ${versionInfo.version} is below the minimum required version. Some features may not work properly. Please upgrade to version ${MIN_CLI_VERSION_FOR_SESSION_METHODS} or later.`,
      );
    }
  } catch (error) {
    console.error('[CliVersionChecker] Failed to check CLI version:', error);
  }
}

/**
 * Process server version information from initialize response
 *
 * @param init - Initialize response object
 */
export function processServerVersion(init: unknown): void {
  try {
    const obj = (init || {}) as Record<string, unknown>;

    // Extract version information from initialize response
    const serverVersion =
      obj['version'] || obj['serverVersion'] || obj['cliVersion'];
    if (serverVersion) {
      console.log(
        '[CliVersionChecker] Server version from initialize response:',
        serverVersion,
      );

      // Update CLI context with version info from server
      const cliContextManager = CliContextManager.getInstance();

      // Create version info directly without async call
      const versionInfo: CliVersionInfo = {
        version: String(serverVersion),
        isSupported: true, // Assume supported for now
        features: {
          supportsSessionList: true,
          supportsSessionLoad: true,
        },
        detectionResult: {
          isInstalled: true,
          version: String(serverVersion),
        },
      };

      cliContextManager.setCurrentVersionInfo(versionInfo);
    }
  } catch (error) {
    console.error(
      '[CliVersionChecker] Failed to process server version:',
      error,
    );
  }
}
