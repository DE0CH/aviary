import * as vscode from 'vscode';
import type { DiffManager } from '../diff-manager.js';
import type { WebViewProvider } from '../webview/WebViewProvider.js';

type Logger = (message: string) => void;

export const runQwenCodeCommand = 'qwen-code.runQwenCode';
export const showDiffCommand = 'qwenCode.showDiff';
export const openChatCommand = 'qwen-code.openChat';
export const openNewChatTabCommand = 'qwenCode.openNewChatTab';
export const loginCommand = 'qwen-code.login';
export const clearAuthCacheCommand = 'qwen-code.clearAuthCache';

export function registerNewCommands(
  context: vscode.ExtensionContext,
  log: Logger,
  diffManager: DiffManager,
  getWebViewProviders: () => WebViewProvider[],
  createWebViewProvider: () => WebViewProvider,
): void {
  const disposables: vscode.Disposable[] = [];

  disposables.push(
    vscode.commands.registerCommand(openChatCommand, async () => {
      const config = vscode.workspace.getConfiguration('qwenCode');
      const useTerminal = config.get<boolean>('useTerminal', false);

      // Use terminal mode
      if (useTerminal) {
        await vscode.commands.executeCommand(
          runQwenCodeCommand,
          vscode.TerminalLocation.Editor, // create a terminal in the editor area,
        );
      } else {
        // Use WebView mode
        const providers = getWebViewProviders();
        if (providers.length > 0) {
          await providers[providers.length - 1].show();
        } else {
          const provider = createWebViewProvider();
          await provider.show();
        }
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

          await diffManager.showDiff(absolutePath, args.oldText, args.newText);
        } catch (error) {
          log(`[Command] Error showing diff: ${error}`);
          vscode.window.showErrorMessage(`Failed to show diff: ${error}`);
        }
      },
    ),
  );

  // TODO: qwenCode.openNewChatTab (not contributed in package.json; used programmatically)
  disposables.push(
    vscode.commands.registerCommand(openNewChatTabCommand, async () => {
      const provider = createWebViewProvider();
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

  disposables.push(
    vscode.commands.registerCommand(clearAuthCacheCommand, async () => {
      const providers = getWebViewProviders();
      for (const provider of providers) {
        await provider.clearAuthCache();
      }
      vscode.window.showInformationMessage(
        'Qwen Code authentication cache cleared. You will need to login again on next connection.',
      );
      log('Auth cache cleared by user');
    }),
  );

  context.subscriptions.push(...disposables);
}
