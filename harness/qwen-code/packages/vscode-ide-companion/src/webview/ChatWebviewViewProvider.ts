/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import * as vscode from 'vscode';
import type { WebViewProvider } from './WebViewProvider.js';

export const CHAT_VIEW_ID_PANEL = 'qwenCode.chatView.panel';
export const CHAT_VIEW_ID_SECONDARY = 'qwenCode.chatView.secondary';

/**
 * WebviewView host for placing the chat UI in panel/secondary sidebar.
 */
export class ChatWebviewViewProvider implements vscode.WebviewViewProvider {
  constructor(
    private readonly webViewProvider: WebViewProvider,
    private readonly extensionUri: vscode.Uri,
    private readonly hostKind: 'panel' | 'secondary',
  ) {}

  async resolveWebviewView(webviewView: vscode.WebviewView): Promise<void> {
    console.log(
      `[ChatWebviewViewProvider] resolveWebviewView invoked for ${webviewView.viewType} (host=${this.hostKind})`,
    );
    // Determine the view ID from the webviewView
    const viewId = webviewView.viewType; // This will be either 'qwenCode.chatView.panel' or 'qwenCode.chatView.secondary'

    // Ensure scripts/resources are allowed
    webviewView.webview.options = {
      enableScripts: true,
      localResourceRoots: [
        vscode.Uri.joinPath(this.extensionUri, 'dist'),
        vscode.Uri.joinPath(this.extensionUri, 'assets'),
      ],
    };

    await this.webViewProvider.attachToView(webviewView, viewId);
  }
}
