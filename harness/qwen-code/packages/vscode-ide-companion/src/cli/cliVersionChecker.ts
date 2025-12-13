/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import * as vscode from 'vscode';
import { CliDetector, type CliDetectionResult } from './cliDetector.js';
import {
  CliVersionManager,
  MIN_CLI_VERSION_FOR_SESSION_METHODS,
} from './cliVersionManager.js';
import semver from 'semver';

/**
 * CLI Version Checker
 *
 * Handles CLI version checking with throttling to prevent frequent notifications.
 * This class manages version checking and provides version information without
 * constantly bothering the user with popups.
 */
export class CliVersionChecker {
  private static instance: CliVersionChecker;
  private lastNotificationTime: number = 0;
  private static readonly NOTIFICATION_COOLDOWN_MS = 300000; // 5 minutes cooldown
  private context: vscode.ExtensionContext;

  private constructor(context: vscode.ExtensionContext) {
    this.context = context;
  }

  /**
   * Get singleton instance
   */
  static getInstance(context?: vscode.ExtensionContext): CliVersionChecker {
    if (!CliVersionChecker.instance && context) {
      CliVersionChecker.instance = new CliVersionChecker(context);
    }
    return CliVersionChecker.instance;
  }

  /**
   * Check CLI version with cooldown to prevent spamming notifications
   *
   * @param showNotifications - Whether to show notifications for issues
   * @returns Promise with version check result
   */
  async checkCliVersion(showNotifications: boolean = true): Promise<{
    isInstalled: boolean;
    version?: string;
    isSupported: boolean;
    needsUpdate: boolean;
    error?: string;
  }> {
    try {
      // Detect CLI installation
      const detectionResult: CliDetectionResult =
        await CliDetector.detectQwenCli();

      if (!detectionResult.isInstalled) {
        if (showNotifications && this.canShowNotification()) {
          vscode.window.showWarningMessage(
            `Qwen Code CLI not found. Please install it using: npm install -g @qwen-code/qwen-code@latest`,
          );
          this.lastNotificationTime = Date.now();
        }

        return {
          isInstalled: false,
          error: detectionResult.error,
          isSupported: false,
          needsUpdate: false,
        };
      }

      // Get version information
      const versionManager = CliVersionManager.getInstance();
      const versionInfo = await versionManager.detectCliVersion();

      const currentVersion = detectionResult.version;
      const isSupported = versionInfo.isSupported;

      // Check if update is needed (version is too old)
      const needsUpdate = currentVersion
        ? !semver.satisfies(
            currentVersion,
            `>=${MIN_CLI_VERSION_FOR_SESSION_METHODS}`,
          )
        : false;

      // Show notification only if needed and within cooldown period
      if (showNotifications && !isSupported && this.canShowNotification()) {
        vscode.window.showWarningMessage(
          `Qwen Code CLI version ${currentVersion} is below the minimum required version. Some features may not work properly. Please upgrade to version ${MIN_CLI_VERSION_FOR_SESSION_METHODS} or later`,
        );
        this.lastNotificationTime = Date.now();
      }

      return {
        isInstalled: true,
        version: currentVersion,
        isSupported,
        needsUpdate,
      };
    } catch (error) {
      console.error('[CliVersionChecker] Version check failed:', error);

      if (showNotifications && this.canShowNotification()) {
        vscode.window.showErrorMessage(
          `Failed to check Qwen Code CLI version: ${error instanceof Error ? error.message : String(error)}`,
        );
        this.lastNotificationTime = Date.now();
      }

      return {
        isInstalled: false,
        error: error instanceof Error ? error.message : String(error),
        isSupported: false,
        needsUpdate: false,
      };
    }
  }

  /**
   * Check if notification can be shown based on cooldown period
   */
  private canShowNotification(): boolean {
    return (
      Date.now() - this.lastNotificationTime >
      CliVersionChecker.NOTIFICATION_COOLDOWN_MS
    );
  }
}
