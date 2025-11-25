/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import * as vscode from 'vscode';
import { CliDetector } from './cliDetector.js';

/**
 * CLI 检测和安装处理器
 * 负责 Qwen CLI 的检测、安装和提示功能
 */
export class CliInstaller {
  /**
   * 检查 CLI 安装状态并发送结果到 WebView
   * @param sendToWebView 发送消息到 WebView 的回调函数
   */
  static async checkInstallation(
    sendToWebView: (message: unknown) => void,
  ): Promise<void> {
    try {
      const result = await CliDetector.detectQwenCli();

      sendToWebView({
        type: 'cliDetectionResult',
        data: {
          isInstalled: result.isInstalled,
          cliPath: result.cliPath,
          version: result.version,
          error: result.error,
          installInstructions: result.isInstalled
            ? undefined
            : CliDetector.getInstallationInstructions(),
        },
      });

      if (!result.isInstalled) {
        console.log('[CliInstaller] Qwen CLI not detected:', result.error);
      } else {
        console.log(
          '[CliInstaller] Qwen CLI detected:',
          result.cliPath,
          result.version,
        );
      }
    } catch (error) {
      console.error('[CliInstaller] CLI detection error:', error);
    }
  }

  /**
   * 提示用户安装 CLI
   * 显示警告消息，提供安装选项
   */
  static async promptInstallation(): Promise<void> {
    const selection = await vscode.window.showWarningMessage(
      'Qwen Code CLI is not installed. You can browse conversation history, but cannot send new messages.',
      'Install Now',
      'View Documentation',
      'Remind Me Later',
    );

    if (selection === 'Install Now') {
      await this.install();
    } else if (selection === 'View Documentation') {
      vscode.env.openExternal(
        vscode.Uri.parse('https://github.com/QwenLM/qwen-code#installation'),
      );
    }
  }

  /**
   * 安装 Qwen CLI
   * 通过 npm 安装全局 CLI 包
   */
  static async install(): Promise<void> {
    try {
      // Show progress notification
      await vscode.window.withProgress(
        {
          location: vscode.ProgressLocation.Notification,
          title: 'Installing Qwen Code CLI',
          cancellable: false,
        },
        async (progress) => {
          progress.report({
            message: 'Running: npm install -g @qwen-code/qwen-code@latest',
          });

          const { exec } = await import('child_process');
          const { promisify } = await import('util');
          const execAsync = promisify(exec);

          try {
            const { stdout, stderr } = await execAsync(
              'npm install -g @qwen-code/qwen-code@latest',
              { timeout: 120000 }, // 2 minutes timeout
            );

            console.log('[CliInstaller] Installation output:', stdout);
            if (stderr) {
              console.warn('[CliInstaller] Installation stderr:', stderr);
            }

            // Clear cache and recheck
            CliDetector.clearCache();
            const detection = await CliDetector.detectQwenCli();

            if (detection.isInstalled) {
              vscode.window
                .showInformationMessage(
                  `✅ Qwen Code CLI installed successfully! Version: ${detection.version}`,
                  'Reload Window',
                )
                .then((selection) => {
                  if (selection === 'Reload Window') {
                    vscode.commands.executeCommand(
                      'workbench.action.reloadWindow',
                    );
                  }
                });
            } else {
              throw new Error(
                'Installation completed but CLI still not detected',
              );
            }
          } catch (error) {
            const errorMessage =
              error instanceof Error ? error.message : String(error);
            console.error('[CliInstaller] Installation failed:', errorMessage);

            vscode.window
              .showErrorMessage(
                `Failed to install Qwen Code CLI: ${errorMessage}`,
                'Try Manual Installation',
                'View Documentation',
              )
              .then((selection) => {
                if (selection === 'Try Manual Installation') {
                  const terminal = vscode.window.createTerminal(
                    'Qwen Code Installation',
                  );
                  terminal.show();
                  terminal.sendText(
                    'npm install -g @qwen-code/qwen-code@latest',
                  );
                } else if (selection === 'View Documentation') {
                  vscode.env.openExternal(
                    vscode.Uri.parse(
                      'https://github.com/QwenLM/qwen-code#installation',
                    ),
                  );
                }
              });
          }
        },
      );
    } catch (error) {
      console.error('[CliInstaller] Install CLI error:', error);
    }
  }
}
