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

// Track which versions have already been warned about to avoid repetitive warnings
// Using a Map with timestamps to allow warnings to be shown again after a certain period
const warnedVersions = new Map<string, number>();
const WARNING_COOLDOWN_MS = 24 * 60 * 60 * 1000; // 24 hours cooldown

/**
 * Check CLI version and show warning if below minimum requirement
 * Provides an "Upgrade Now" option for unsupported versions
 *
 * @returns Version information
 */
export async function checkCliVersionAndWarn(): Promise<CliVersionInfo> {
  try {
    const cliContextManager = CliContextManager.getInstance();
    const versionInfo =
      await CliVersionManager.getInstance().detectCliVersion(true);
    cliContextManager.setCurrentVersionInfo(versionInfo);

    if (!versionInfo.isSupported) {
      // Only show warning if we haven't already warned about this specific version recently
      const versionKey = versionInfo.version || 'unknown';
      const lastWarningTime = warnedVersions.get(versionKey);
      const currentTime = Date.now();

      // Show warning if we haven't warned about this version or if enough time has passed
      if (
        !lastWarningTime ||
        currentTime - lastWarningTime > WARNING_COOLDOWN_MS
      ) {
        // Wait to determine release version number
        const selection = await vscode.window.showWarningMessage(
          `Qwen Code CLI version ${versionInfo.version} is below the minimum required version. Some features may not work properly. Please upgrade to version ${MIN_CLI_VERSION_FOR_SESSION_METHODS} or later.`,
          'Upgrade Now',
        );

        // Handle the user's selection
        if (selection === 'Upgrade Now') {
          // Open terminal and run npm install command
          const terminal = vscode.window.createTerminal(
            'Qwen Code CLI Upgrade',
          );
          terminal.show();
          terminal.sendText('npm install -g @qwen-code/qwen-code@latest');
        }

        // Update the last warning time
        warnedVersions.set(versionKey, currentTime);
      }
    }

    return versionInfo;
  } catch (error) {
    console.error('[CliVersionChecker] Failed to check CLI version:', error);
    // Return a default version info in case of error
    return {
      version: undefined,
      isSupported: false,
      features: {
        supportsSessionList: false,
        supportsSessionLoad: false,
      },
      detectionResult: {
        isInstalled: false,
        error: error instanceof Error ? error.message : String(error),
      },
    };
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
