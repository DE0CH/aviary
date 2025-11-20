/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import * as vscode from 'vscode';

/**
 * Panel 和 Tab 管理器
 * 负责管理 WebView Panel 的创建、显示和 Tab 跟踪
 */
export class PanelManager {
  private panel: vscode.WebviewPanel | null = null;
  private panelTab: vscode.Tab | null = null;

  constructor(
    private extensionUri: vscode.Uri,
    private onPanelDispose: () => void,
  ) {}

  /**
   * 获取当前的 Panel
   */
  getPanel(): vscode.WebviewPanel | null {
    return this.panel;
  }

  /**
   * 设置 Panel（用于恢复）
   */
  setPanel(panel: vscode.WebviewPanel): void {
    this.panel = panel;
  }

  /**
   * 创建新的 WebView Panel
   * @returns 是否是新创建的 Panel
   */
  createPanel(): boolean {
    if (this.panel) {
      return false; // Panel already exists
    }

    // Find if there's already a Qwen Code webview tab open and get its view column
    const existingQwenViewColumn = this.findExistingQwenCodeViewColumn();

    // If we found an existing Qwen Code tab, open in the same view column
    // Otherwise, open beside the active editor
    const targetViewColumn = existingQwenViewColumn ?? vscode.ViewColumn.Beside;

    this.panel = vscode.window.createWebviewPanel(
      'qwenCode.chat',
      'Qwen Code',
      {
        viewColumn: targetViewColumn,
        preserveFocus: false, // Focus the new tab
      },
      {
        enableScripts: true,
        retainContextWhenHidden: true,
        localResourceRoots: [
          vscode.Uri.joinPath(this.extensionUri, 'dist'),
          vscode.Uri.joinPath(this.extensionUri, 'assets'),
        ],
      },
    );

    // Set panel icon to Qwen logo
    this.panel.iconPath = vscode.Uri.joinPath(
      this.extensionUri,
      'assets',
      'icon.png',
    );

    return true; // New panel created
  }

  /**
   * 查找已存在的 Qwen Code webview 所在的 view column
   * @returns 找到的 view column，如果没有则返回 undefined
   */
  private findExistingQwenCodeViewColumn(): vscode.ViewColumn | undefined {
    const allTabs = vscode.window.tabGroups.all.flatMap((g) => g.tabs);

    for (const tab of allTabs) {
      const input: unknown = (tab as { input?: unknown }).input;
      const isWebviewInput = (inp: unknown): inp is { viewType: string } =>
        !!inp && typeof inp === 'object' && 'viewType' in inp;

      if (isWebviewInput(input) && input.viewType === 'qwenCode.chat') {
        // Found an existing Qwen Code tab, get its view column
        const tabGroup = vscode.window.tabGroups.all.find((g) =>
          g.tabs.includes(tab),
        );
        return tabGroup?.viewColumn;
      }
    }

    return undefined;
  }

  /**
   * 自动锁定编辑器组（仅在新创建 Panel 时调用）
   */
  async autoLockEditorGroup(): Promise<void> {
    if (!this.panel) {
      return;
    }

    console.log('[PanelManager] Auto-locking editor group for Qwen Code chat');
    try {
      // Reveal panel without preserving focus to make it the active group
      this.revealPanel(false);

      await vscode.commands.executeCommand('workbench.action.lockEditorGroup');
      console.log('[PanelManager] Editor group locked successfully');
    } catch (error) {
      console.warn('[PanelManager] Failed to lock editor group:', error);
      // Non-fatal error, continue anyway
    }
  }

  /**
   * 显示 Panel（如果存在则 reveal，否则什么都不做）
   * @param preserveFocus 是否保持焦点
   */
  revealPanel(preserveFocus: boolean = true): void {
    if (this.panel) {
      this.panel.reveal(vscode.ViewColumn.Beside, preserveFocus);
    }
  }

  /**
   * 捕获与 WebView Panel 对应的 Tab
   * 用于跟踪和管理 Tab 状态
   */
  captureTab(): void {
    if (!this.panel) {
      return;
    }

    // Defer slightly so the tab model is updated after create/reveal
    setTimeout(() => {
      const allTabs = vscode.window.tabGroups.all.flatMap((g) => g.tabs);
      const match = allTabs.find((t) => {
        // Type guard for webview tab input
        const input: unknown = (t as { input?: unknown }).input;
        const isWebviewInput = (inp: unknown): inp is { viewType: string } =>
          !!inp && typeof inp === 'object' && 'viewType' in inp;
        const isWebview = isWebviewInput(input);
        const sameViewType = isWebview && input.viewType === 'qwenCode.chat';
        const sameLabel = t.label === this.panel!.title;
        return !!(sameViewType || sameLabel);
      });
      this.panelTab = match ?? null;
    }, 50);
  }

  /**
   * 注册 Panel 的 dispose 事件处理器
   * @param disposables 用于存储 Disposable 的数组
   */
  registerDisposeHandler(disposables: vscode.Disposable[]): void {
    if (!this.panel) {
      return;
    }

    this.panel.onDidDispose(
      () => {
        this.panel = null;
        this.panelTab = null;
        this.onPanelDispose();
      },
      null,
      disposables,
    );
  }

  /**
   * 注册视图状态变化事件处理器
   * @param disposables 用于存储 Disposable 的数组
   */
  registerViewStateChangeHandler(disposables: vscode.Disposable[]): void {
    if (!this.panel) {
      return;
    }

    this.panel.onDidChangeViewState(
      () => {
        if (this.panel && this.panel.visible) {
          this.captureTab();
        }
      },
      null,
      disposables,
    );
  }

  /**
   * 销毁 Panel
   */
  dispose(): void {
    this.panel?.dispose();
    this.panel = null;
    this.panelTab = null;
  }
}
