/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import * as vscode from 'vscode';
import { escapeHtml } from '../utils/webviewUtils.js';

/**
 * WebView HTML 内容生成器
 * 负责生成 WebView 的 HTML 内容
 */
export class WebViewContent {
  /**
   * 生成 WebView 的 HTML 内容
   * @param panel WebView Panel
   * @param extensionUri 扩展的 URI
   * @returns HTML 字符串
   */
  static generate(
    panel: vscode.WebviewPanel,
    extensionUri: vscode.Uri,
  ): string {
    const scriptUri = panel.webview.asWebviewUri(
      vscode.Uri.joinPath(extensionUri, 'dist', 'webview.js'),
    );

    // Convert extension URI for webview access - this allows frontend to construct resource paths
    const extensionUriForWebview = panel.webview.asWebviewUri(extensionUri);

    // 对 URI 进行 HTML 转义以防止潜在的注入攻击
    const safeExtensionUri = escapeHtml(extensionUriForWebview.toString());
    const safeScriptUri = escapeHtml(scriptUri.toString());

    return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src ${panel.webview.cspSource}; script-src ${panel.webview.cspSource}; style-src ${panel.webview.cspSource} 'unsafe-inline';">
  <title>Qwen Code Chat</title>
</head>
<body data-extension-uri="${safeExtensionUri}">
  <div id="root"></div>
  <script src="${safeScriptUri}"></script>
</body>
</html>`;
  }
}
