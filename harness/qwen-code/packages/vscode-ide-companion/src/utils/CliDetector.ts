/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { exec } from 'child_process';
import { promisify } from 'util';

const execAsync = promisify(exec);

export interface CliDetectionResult {
  isInstalled: boolean;
  cliPath?: string;
  version?: string;
  error?: string;
}

/**
 * Detects if Qwen Code CLI is installed and accessible
 */
export class CliDetector {
  private static cachedResult: CliDetectionResult | null = null;
  private static lastCheckTime: number = 0;
  private static readonly CACHE_DURATION_MS = 30000; // 30 seconds

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
      this.cachedResult &&
      now - this.lastCheckTime < this.CACHE_DURATION_MS
    ) {
      return this.cachedResult;
    }

    try {
      const isWindows = process.platform === 'win32';
      const whichCommand = isWindows ? 'where' : 'which';

      // Check if qwen command exists
      try {
        const { stdout } = await execAsync(`${whichCommand} qwen`, {
          timeout: 5000,
        });
        const cliPath = stdout.trim().split('\n')[0];

        // Try to get version
        let version: string | undefined;
        try {
          const { stdout: versionOutput } = await execAsync('qwen --version', {
            timeout: 5000,
          });
          version = versionOutput.trim();
        } catch {
          // Version check failed, but CLI is installed
        }

        this.cachedResult = {
          isInstalled: true,
          cliPath,
          version,
        };
        this.lastCheckTime = now;
        return this.cachedResult;
      } catch (_error) {
        // CLI not found
        this.cachedResult = {
          isInstalled: false,
          error: `Qwen Code CLI not found in PATH. Please install it using: npm install -g @qwen-code/qwen-code@latest`,
        };
        this.lastCheckTime = now;
        return this.cachedResult;
      }
    } catch (error) {
      const errorMessage =
        error instanceof Error ? error.message : String(error);
      this.cachedResult = {
        isInstalled: false,
        error: `Failed to detect Qwen Code CLI: ${errorMessage}`,
      };
      this.lastCheckTime = now;
      return this.cachedResult;
    }
  }

  /**
   * Clears the cached detection result
   */
  static clearCache(): void {
    this.cachedResult = null;
    this.lastCheckTime = 0;
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
}
