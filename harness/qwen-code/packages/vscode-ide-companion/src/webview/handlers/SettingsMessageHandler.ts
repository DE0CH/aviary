/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import * as vscode from 'vscode';
import { BaseMessageHandler } from './BaseMessageHandler.js';

/**
 * Settings message handler
 * Handles all settings-related messages
 */
export class SettingsMessageHandler extends BaseMessageHandler {
  canHandle(messageType: string): boolean {
    return ['openSettings', 'recheckCli', 'setApprovalMode'].includes(
      messageType,
    );
  }

  async handle(message: { type: string; data?: unknown }): Promise<void> {
    switch (message.type) {
      case 'openSettings':
        await this.handleOpenSettings();
        break;

      case 'recheckCli':
        await this.handleRecheckCli();
        break;

      case 'setApprovalMode':
        await this.handleSetApprovalMode(
          message.data as {
            modeId?: 'plan' | 'default' | 'auto-edit' | 'yolo';
          },
        );
        break;

      default:
        console.warn(
          '[SettingsMessageHandler] Unknown message type:',
          message.type,
        );
        break;
    }
  }

  /**
   * Open settings page
   */
  private async handleOpenSettings(): Promise<void> {
    try {
      // Open settings in a side panel
      await vscode.commands.executeCommand('workbench.action.openSettings', {
        query: 'qwenCode',
      });
    } catch (error) {
      console.error('[SettingsMessageHandler] Failed to open settings:', error);
      vscode.window.showErrorMessage(`Failed to open settings: ${error}`);
    }
  }

  /**
   * Recheck CLI
   */
  private async handleRecheckCli(): Promise<void> {
    try {
      await vscode.commands.executeCommand('qwenCode.recheckCli');
      this.sendToWebView({
        type: 'cliRechecked',
        data: { success: true },
      });
    } catch (error) {
      console.error('[SettingsMessageHandler] Failed to recheck CLI:', error);
      this.sendToWebView({
        type: 'error',
        data: { message: `Failed to recheck CLI: ${error}` },
      });
    }
  }

  /**
   * Set approval mode via agent (ACP session/set_mode)
   */
  private async handleSetApprovalMode(data?: {
    modeId?: 'plan' | 'default' | 'auto-edit' | 'yolo';
  }): Promise<void> {
    try {
      const modeId = (data?.modeId || 'default') as
        | 'plan'
        | 'default'
        | 'auto-edit'
        | 'yolo';
      await this.agentManager.setApprovalModeFromUi(
        modeId === 'plan'
          ? 'plan'
          : modeId === 'auto-edit'
            ? 'auto'
            : modeId === 'yolo'
              ? 'yolo'
              : 'ask',
      );
      // No explicit response needed; WebView listens for modeChanged
    } catch (error) {
      console.error('[SettingsMessageHandler] Failed to set mode:', error);
      this.sendToWebView({
        type: 'error',
        data: { message: `Failed to set mode: ${error}` },
      });
    }
  }
}
