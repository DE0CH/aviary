import * as vscode from 'vscode';
import type { DiffManager } from '../diff-manager.js';
import type { WebViewProvider } from '../webview/WebViewProvider.js';

type Logger = (message: string) => void;

export const showDiffCommand = 'qwenCode.showDiff';
export const openChatCommand = 'qwenCode.openChat';

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
      console.log('[Command] Using terminal mode:', useTerminal);
      if (useTerminal) {
        // 使用终端模式
        await vscode.commands.executeCommand(
          'qwen-code.runQwenCode',
          vscode.TerminalLocation.Editor, // 在编辑器区域创建终端,
        );
      } else {
        // 使用 WebView 模式
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
        log(`[Command] showDiff called for: ${args.path}`);
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
    vscode.commands.registerCommand('qwenCode.openNewChatTab', async () => {
      const provider = createWebViewProvider();
      await provider.show();
    }),
  );

  disposables.push(
    vscode.commands.registerCommand('qwenCode.clearAuthCache', async () => {
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

  disposables.push(
    vscode.commands.registerCommand('qwenCode.login', async () => {
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

  context.subscriptions.push(...disposables);
}
