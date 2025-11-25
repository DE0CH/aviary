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
    console.log('[PanelManager] Setting panel for restoration');
    this.panel = panel;
  }

  /**
   * 创建新的 WebView Panel
   * @returns 是否是新创建的 Panel
   */
  async createPanel(): Promise<boolean> {
    if (this.panel) {
      return false; // Panel already exists
    }

    // Find if there's already a Qwen Code webview tab open and get its view column
    const existingQwenInfo = this.findExistingQwenCodeGroup();

    // If we found an existing Qwen Code tab, open in the same view column
    // Otherwise, open beside the active editor
    const targetViewColumn =
      existingQwenInfo?.viewColumn ?? vscode.ViewColumn.Beside;
    console.log('[PanelManager] existingQwenInfo', existingQwenInfo);
    console.log('[PanelManager] targetViewColumn', targetViewColumn);

    // If there's an existing Qwen Code group, ensure it's unlocked so we can add new tabs
    // We try to unlock regardless of current state - if already unlocked, this is a no-op
    if (existingQwenInfo?.group) {
      console.log(
        "[PanelManager] Found existing Qwen Code group, ensuring it's unlocked...",
      );

      try {
        // We need to make the target group active first
        // Find a Qwen Code tab in that group
        const firstQwenTab = existingQwenInfo.group.tabs.find((tab) => {
          const input: unknown = (tab as { input?: unknown }).input;
          const isWebviewInput = (inp: unknown): inp is { viewType: string } =>
            !!inp && typeof inp === 'object' && 'viewType' in inp;
          return (
            isWebviewInput(input) &&
            input.viewType === 'mainThreadWebview-qwenCode.chat'
          );
        });

        if (firstQwenTab) {
          // Make the group active by focusing on one of its tabs
          const activeTabGroup = vscode.window.tabGroups.activeTabGroup;
          if (activeTabGroup !== existingQwenInfo.group) {
            // Switch to the target group
            await vscode.commands.executeCommand(
              'workbench.action.focusFirstEditorGroup',
            );
          }
        }

        // Try to unlock the group (will be no-op if already unlocked)
        await vscode.commands.executeCommand(
          'workbench.action.unlockEditorGroup',
        );
        console.log('[PanelManager] Unlock command executed');
      } catch (error) {
        console.warn(
          '[PanelManager] Failed to unlock group, continuing anyway:',
          error,
        );
        // Continue anyway - the group might not be locked
      }
    }

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
   * 查找已存在的 Qwen Code webview 所在的 group 和 view column
   * @returns 找到的 group 和 view column，如果没有则返回 undefined
   */
  private findExistingQwenCodeGroup():
    | { group: vscode.TabGroup; viewColumn: vscode.ViewColumn }
    | undefined {
    for (const group of vscode.window.tabGroups.all) {
      for (const tab of group.tabs) {
        const input: unknown = (tab as { input?: unknown }).input;
        const isWebviewInput = (inp: unknown): inp is { viewType: string } =>
          !!inp && typeof inp === 'object' && 'viewType' in inp;

        if (
          isWebviewInput(input) &&
          input.viewType === 'mainThreadWebview-qwenCode.chat'
        ) {
          // Found an existing Qwen Code tab
          console.log('[PanelManager] Found existing Qwen Code group:', {
            viewColumn: group.viewColumn,
            tabCount: group.tabs.length,
            isActive: group.isActive,
          });
          return {
            group,
            viewColumn: group.viewColumn,
          };
        }
      }
    }

    return undefined;
  }

  /**
   * 自动锁定编辑器组（仅在新创建 Panel 时调用）
   * 注意：我们不再自动锁定 Qwen Code group，以允许用户创建多个 Qwen Code tab
   */
  async autoLockEditorGroup(): Promise<void> {
    if (!this.panel) {
      return;
    }

    // We don't auto-lock anymore to allow multiple Qwen Code tabs in the same group
    console.log(
      '[PanelManager] Skipping auto-lock to allow multiple Qwen Code tabs',
    );
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
