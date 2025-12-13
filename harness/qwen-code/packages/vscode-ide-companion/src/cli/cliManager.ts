/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import * as vscode from 'vscode';
import { exec } from 'child_process';
import { promisify } from 'util';
import semver from 'semver';
import { CliInstaller } from './cliInstaller.js';

const execAsync = promisify(exec);

export const MIN_CLI_VERSION_FOR_SESSION_METHODS = '0.5.0';

export interface CliDetectionResult {
  isInstalled: boolean;
  cliPath?: string;
  version?: string;
  error?: string;
}

export interface CliFeatureFlags {
  supportsSessionList: boolean;
  supportsSessionLoad: boolean;
}

export interface CliVersionInfo {
  version: string | undefined;
  isSupported: boolean;
  features: CliFeatureFlags;
  detectionResult: CliDetectionResult;
}

export class CliManager {
  private static instance: CliManager;
  private lastNotificationTime: number = 0;
  private static readonly NOTIFICATION_COOLDOWN_MS = 300000; // 5 minutes cooldown
  private context: vscode.ExtensionContext | undefined;

  // Cache mechanisms
  private static cachedDetectionResult: CliDetectionResult | null = null;
  private static detectionLastCheckTime: number = 0;
  private cachedVersionInfo: CliVersionInfo | null = null;
  private versionLastCheckTime: number = 0;
  private static readonly CACHE_DURATION_MS = 30000; // 30 seconds

  private constructor(context?: vscode.ExtensionContext) {
    this.context = context;
  }

  /**
   * Get singleton instance
   */
  static getInstance(context?: vscode.ExtensionContext): CliManager {
    if (!CliManager.instance && context) {
      CliManager.instance = new CliManager(context);
    }
    return CliManager.instance;
  }

  /**
   * Checks if the Qwen Code CLI is installed
   * @param forceRefresh - Force a new check, ignoring cache
   * @returns Detection result with installation status and details
   */
  static async detectQwenCli(
    forceRefresh = false,
  ): Promise<CliDetectionResult> {
    const now = Date.now();

    // Return cached result if available and not expired
    if (
      !forceRefresh &&
      this.cachedDetectionResult &&
      now - this.detectionLastCheckTime < this.CACHE_DURATION_MS
    ) {
      console.log('[CliManager] Returning cached detection result');
      return this.cachedDetectionResult;
    }

    console.log(
      '[CliManager] Starting CLI detection, current PATH:',
      process.env.PATH,
    );

    try {
      const isWindows = process.platform === 'win32';
      const whichCommand = isWindows ? 'where' : 'which';

      // Check if qwen command exists
      try {
        // Use NVM environment for consistent detection
        // Fallback chain: default alias -> node alias -> current version
        const detectionCommand =
          process.platform === 'win32'
            ? `${whichCommand} qwen`
            : 'source ~/.nvm/nvm.sh 2>/dev/null && (nvm use default 2>/dev/null || nvm use node 2>/dev/null || nvm use 2>/dev/null); which qwen';

        console.log(
          '[CliManager] Detecting CLI with command:',
          detectionCommand,
        );

        const { stdout } = await execAsync(detectionCommand, {
          timeout: 5000,
          shell: isWindows ? undefined : '/bin/bash',
        });
        // The output may contain multiple lines, with NVM activation messages
        // We want the last line which should be the actual path
        const lines = stdout
          .trim()
          .split('\n')
          .filter((line) => line.trim());
        const cliPath = lines[lines.length - 1];

        console.log('[CliManager] Found CLI at:', cliPath);

        // Try to get version
        let version: string | undefined;
        try {
          // Use NVM environment for version check
          // Fallback chain: default alias -> node alias -> current version
          // Also ensure we use the correct Node.js version that matches the CLI installation
          const versionCommand =
            process.platform === 'win32'
              ? 'qwen --version'
              : 'source ~/.nvm/nvm.sh 2>/dev/null && (nvm use default 2>/dev/null || nvm use node 2>/dev/null || nvm use 2>/dev/null); qwen --version';

          console.log(
            '[CliManager] Getting version with command:',
            versionCommand,
          );

          const { stdout: versionOutput } = await execAsync(versionCommand, {
            timeout: 5000,
            shell: isWindows ? undefined : '/bin/bash',
          });
          // The output may contain multiple lines, with NVM activation messages
          // We want the last line which should be the actual version
          const versionLines = versionOutput
            .trim()
            .split('\n')
            .filter((line) => line.trim());
          version = versionLines[versionLines.length - 1];
          console.log('[CliManager] CLI version:', version);
        } catch (versionError) {
          console.log('[CliManager] Failed to get CLI version:', versionError);
          // Version check failed, but CLI is installed
        }

        this.cachedDetectionResult = {
          isInstalled: true,
          cliPath,
          version,
        };
        this.detectionLastCheckTime = now;
        return this.cachedDetectionResult;
      } catch (detectionError) {
        console.log('[CliManager] CLI not found, error:', detectionError);
        // CLI not found
        let error = `Qwen Code CLI not found in PATH. Please install it using: npm install -g @qwen-code/qwen-code@latest`;

        // Provide specific guidance for permission errors
        if (detectionError instanceof Error) {
          const errorMessage = detectionError.message;
          if (
            errorMessage.includes('EACCES') ||
            errorMessage.includes('Permission denied')
          ) {
            error += `\n\nThis may be due to permission issues. Possible solutions:
              \n1. Reinstall the CLI without sudo: npm install -g @qwen-code/qwen-code@latest
              \n2. If previously installed with sudo, fix ownership: sudo chown -R $(whoami) $(npm config get prefix)/lib/node_modules/@qwen-code/qwen-code
              \n3. Use nvm for Node.js version management to avoid permission issues
              \n4. Check your PATH environment variable includes npm's global bin directory`;
          }
        }

        this.cachedDetectionResult = {
          isInstalled: false,
          error,
        };
        this.detectionLastCheckTime = now;
        return this.cachedDetectionResult;
      }
    } catch (error) {
      console.log('[CliManager] General detection error:', error);
      const errorMessage =
        error instanceof Error ? error.message : String(error);

      let userFriendlyError = `Failed to detect Qwen Code CLI: ${errorMessage}`;

      // Provide specific guidance for permission errors
      if (
        errorMessage.includes('EACCES') ||
        errorMessage.includes('Permission denied')
      ) {
        userFriendlyError += `\n\nThis may be due to permission issues. Possible solutions:
          \n1. Reinstall the CLI without sudo: npm install -g @qwen-code/qwen-code@latest
          \n2. If previously installed with sudo, fix ownership: sudo chown -R $(whoami) $(npm config get prefix)/lib/node_modules/@qwen-code/qwen-code
          \n3. Use nvm for Node.js version management to avoid permission issues
          \n4. Check your PATH environment variable includes npm's global bin directory`;
      }

      this.cachedDetectionResult = {
        isInstalled: false,
        error: userFriendlyError,
      };
      this.detectionLastCheckTime = now;
      return this.cachedDetectionResult;
    }
  }

  /**
   * Clears the cached detection result
   */
  static clearCache(): void {
    this.cachedDetectionResult = null;
    this.detectionLastCheckTime = 0;
  }

  /**
   * Gets installation instructions based on the platform
   */
  static getInstallationInstructions(): {
    title: string;
    steps: string[];
    documentationUrl: string;
  } {
    return {
      title: 'Qwen Code CLI is not installed',
      steps: [
        'Install via npm:',
        '  npm install -g @qwen-code/qwen-code@latest',
        '',
        'If you are using nvm (automatically handled by the plugin):',
        '  The plugin will automatically use your default nvm version',
        '',
        'Or install from source:',
        '  git clone https://github.com/QwenLM/qwen-code.git',
        '  cd qwen-code',
        '  npm install',
        '  npm install -g .',
        '',
        'After installation, reload VS Code or restart the extension.',
      ],
      documentationUrl: 'https://github.com/QwenLM/qwen-code#installation',
    };
  }

  /**
   * Check if CLI version meets minimum requirements
   *
   * @param version - Version string to check
   * @param minVersion - Minimum required version
   * @returns Whether version meets requirements
   */
  private isVersionSupported(
    version: string | undefined,
    minVersion: string,
  ): boolean {
    if (!version) {
      return false;
    }

    // Use semver for robust comparison (handles v-prefix, pre-release, etc.)
    const v = semver.valid(version) ?? semver.coerce(version)?.version ?? null;
    const min =
      semver.valid(minVersion) ?? semver.coerce(minVersion)?.version ?? null;

    if (!v || !min) {
      console.warn(
        `[CliManager] Invalid semver: version=${version}, min=${minVersion}`,
      );
      return false;
    }
    console.log(`[CliManager] Version ${v} meets requirements: ${min}`);
    return semver.gte(v, min);
  }

  /**
   * Get feature flags based on CLI version
   *
   * @param version - CLI version string
   * @returns Feature flags
   */
  private getFeatureFlags(version: string | undefined): CliFeatureFlags {
    const isSupportedVersion = this.isVersionSupported(
      version,
      MIN_CLI_VERSION_FOR_SESSION_METHODS,
    );

    return {
      supportsSessionList: isSupportedVersion,
      supportsSessionLoad: isSupportedVersion,
    };
  }

  /**
   * Detect CLI version and features
   *
   * @param forceRefresh - Force a new check, ignoring cache
   * @returns CLI version information
   */
  async detectCliVersion(forceRefresh = false): Promise<CliVersionInfo> {
    const now = Date.now();

    // Return cached result if available and not expired
    if (
      !forceRefresh &&
      this.cachedVersionInfo &&
      now - this.versionLastCheckTime < CliManager.CACHE_DURATION_MS
    ) {
      console.log('[CliManager] Returning cached version info');
      return this.cachedVersionInfo;
    }

    console.log('[CliManager] Detecting CLI version...');

    try {
      // Detect CLI installation
      const detectionResult = await CliManager.detectQwenCli(forceRefresh);

      const versionInfo: CliVersionInfo = {
        version: detectionResult.version,
        isSupported: this.isVersionSupported(
          detectionResult.version,
          MIN_CLI_VERSION_FOR_SESSION_METHODS,
        ),
        features: this.getFeatureFlags(detectionResult.version),
        detectionResult,
      };

      // Cache the result
      this.cachedVersionInfo = versionInfo;
      this.versionLastCheckTime = now;

      console.log('[CliManager] CLI version detection result:', versionInfo);

      return versionInfo;
    } catch (error) {
      console.error('[CliManager] Failed to detect CLI version:', error);

      // Return fallback result
      const fallbackResult: CliVersionInfo = {
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

      return fallbackResult;
    }
  }

  /**
   * Clear cached version information
   */
  clearVersionCache(): void {
    this.cachedVersionInfo = null;
    this.versionLastCheckTime = 0;
    CliManager.clearCache();
  }

  /**
   * Check if CLI supports session/list method
   *
   * @param forceRefresh - Force a new check, ignoring cache
   * @returns Whether session/list is supported
   */
  async supportsSessionList(forceRefresh = false): Promise<boolean> {
    const versionInfo = await this.detectCliVersion(forceRefresh);
    return versionInfo.features.supportsSessionList;
  }

  /**
   * Check if CLI supports session/load method
   *
   * @param forceRefresh - Force a new check, ignoring cache
   * @returns Whether session/load is supported
   */
  async supportsSessionLoad(forceRefresh = false): Promise<boolean> {
    const versionInfo = await this.detectCliVersion(forceRefresh);
    return versionInfo.features.supportsSessionLoad;
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
        await CliManager.detectQwenCli();

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
      const versionInfo = await this.detectCliVersion();

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
        vscode.window
          .showWarningMessage(
            `Qwen Code CLI version ${currentVersion} is below the minimum required version. Some features may not work properly. Please upgrade to version ${MIN_CLI_VERSION_FOR_SESSION_METHODS} or later`,
            'Upgrade Now',
            'View Documentation',
          )
          .then(async (selection) => {
            if (selection === 'Upgrade Now') {
              await CliInstaller.install();
            } else if (selection === 'View Documentation') {
              vscode.env.openExternal(
                vscode.Uri.parse(
                  'https://github.com/QwenLM/qwen-code#installation',
                ),
              );
            }
          });
        this.lastNotificationTime = Date.now();
      }

      return {
        isInstalled: true,
        version: currentVersion,
        isSupported,
        needsUpdate,
      };
    } catch (error) {
      console.error('[CliManager] Version check failed:', error);

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
      CliManager.NOTIFICATION_COOLDOWN_MS
    );
  }
}
