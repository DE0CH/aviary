import * as vscode from 'vscode';
import type { DiffManager } from '../diff-manager.js';
import type { WebViewProvider } from '../webview/WebViewProvider.js';

type Logger = (message: string) => void;
export type ChatHostLocation = 'editor' | 'panel' | 'secondary';

export const runQwenCodeCommand = 'qwen-code.runQwenCode';
export const showDiffCommand = 'qwenCode.showDiff';
export const openChatCommand = 'qwen-code.openChat';
export const openNewChatTabCommand = 'qwenCode.openNewChatTab';
export const loginCommand = 'qwen-code.login';
export const setChatLocationEditorCommand = 'qwen-code.setChatLocation.editor';
export const setChatLocationPanelCommand = 'qwen-code.setChatLocation.panel';
export const setChatLocationSecondaryCommand =
  'qwen-code.setChatLocation.secondary';

export function registerNewCommands(
  context: vscode.ExtensionContext,
  log: Logger,
  diffManager: DiffManager,
  getWebViewProviders: () => WebViewProvider[],
  createWebViewProvider: () => WebViewProvider,
  getChatHostLocation: () => ChatHostLocation,
  focusChatView: () => Promise<void>,
): void {
  const disposables: vscode.Disposable[] = [];

  disposables.push(
    vscode.commands.registerCommand(openChatCommand, async () => {
      const host = getChatHostLocation();
      if (host !== 'editor') {
        await focusChatView();
        return;
      }

      const providers = getWebViewProviders();
      if (providers.length > 0) {
        await providers[providers.length - 1].show();
      } else {
        const provider = createWebViewProvider();
        await provider.show();
      }
    }),
  );

  disposables.push(
    vscode.commands.registerCommand(
      showDiffCommand,
      async (args: { path: string; oldText: string; newText: string }) => {
        try {
          let absolutePath = args.path;
          if (!args.path.startsWith('/') && !args.path.match(/^[a-zA-Z]:/)) {
            const workspaceFolder = vscode.workspace.workspaceFolders?.[0];
            if (workspaceFolder) {
              absolutePath = vscode.Uri.joinPath(
                workspaceFolder.uri,
                args.path,
              ).fsPath;
            }
          }
          log(`[Command] Showing diff for ${absolutePath}`);
          await diffManager.showDiff(absolutePath, args.oldText, args.newText);
        } catch (error) {
          log(`[Command] Error showing diff: ${error}`);
          vscode.window.showErrorMessage(`Failed to show diff: ${error}`);
        }
      },
    ),
  );

  disposables.push(
    vscode.commands.registerCommand(openNewChatTabCommand, async () => {
      const host = getChatHostLocation();
      if (host !== 'editor') {
        vscode.window.showInformationMessage(
          '当前配置使用面板/Secondary Bar 承载聊天，暂不支持多标签。将为你聚焦现有聊天视图。',
        );
        await focusChatView();
        return;
      }

      const provider = createWebViewProvider();
      // Session restoration is now disabled by default, so no need to suppress it
      await provider.show();
    }),
  );

  disposables.push(
    vscode.commands.registerCommand(loginCommand, async () => {
      const providers = getWebViewProviders();
      if (providers.length > 0) {
        await providers[providers.length - 1].forceReLogin();
      } else {
        vscode.window.showInformationMessage(
          'Please open Qwen Code chat first before logging in.',
        );
      }
    }),
  );

  const updateChatLocation = async (location: ChatHostLocation) => {
    const current = getChatHostLocation();
    if (current === location) {
      log(`[Command] Chat location unchanged (${location}), focusing view`);
      await focusChatView();
      return;
    }

    const target: vscode.ConfigurationTarget =
      vscode.workspace.workspaceFolders?.length && vscode.workspace.name
        ? vscode.ConfigurationTarget.Workspace
        : vscode.ConfigurationTarget.Global;

    try {
      await vscode.workspace
        .getConfiguration('qwen-code')
        .update('chat.location', location, target);
      log(
        `[Command] Chat location set to ${location} (target=${target}; prev=${current})`,
      );
      // Update context immediately so view container visibility switches without reload
      await vscode.commands.executeCommand(
        'setContext',
        'qwenCode.chatLocation',
        location,
      );

      // Dispose existing editor-hosted chat panels when moving to panel/secondary to avoid confusion.
      if (location !== 'editor') {
        for (const provider of getWebViewProviders()) {
          try {
            provider.getPanel()?.dispose();
          } catch (error) {
            log(
              `[Command] Failed to dispose chat panel during relocation: ${error}`,
            );
          }
        }
      }

      // Small delay to ensure context has updated before focusing
      await new Promise((resolve) => setTimeout(resolve, 100));
      await focusChatView();
      void vscode.window.showInformationMessage(
        `聊天位置已切换为 ${location === 'editor' ? '编辑器标签页' : location === 'panel' ? '底部面板' : 'Secondary Bar'}，已尝试为你打开对应位置。如未生效，请重载窗口。`,
      );
    } catch (error) {
      const msg = error instanceof Error ? error.message : String(error);
      log(`[Command] Failed to set chat location: ${msg}`);
      void vscode.window.showErrorMessage(
        `切换聊天位置失败：${msg}。可在 Settings 搜索 "Qwen Code Chat Location" 手动修改。`,
      );
    }
  };

  disposables.push(
    vscode.commands.registerCommand(setChatLocationEditorCommand, async () => {
      await updateChatLocation('editor');
    }),
    vscode.commands.registerCommand(setChatLocationPanelCommand, async () => {
      await updateChatLocation('panel');
    }),
    vscode.commands.registerCommand(
      setChatLocationSecondaryCommand,
      async () => {
        await updateChatLocation('secondary');
      },
    ),
  );
  context.subscriptions.push(...disposables);
}
