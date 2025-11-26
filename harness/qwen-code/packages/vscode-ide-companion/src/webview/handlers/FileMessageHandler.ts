/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import * as vscode from 'vscode';
import { BaseMessageHandler } from './BaseMessageHandler.js';
import { getFileName } from '../utils/webviewUtils.js';

/**
 * File message handler
 * Handles all file-related messages
 */
export class FileMessageHandler extends BaseMessageHandler {
  canHandle(messageType: string): boolean {
    return [
      'attachFile',
      'showContextPicker',
      'getWorkspaceFiles',
      'openFile',
      'openDiff',
    ].includes(messageType);
  }

  async handle(message: { type: string; data?: unknown }): Promise<void> {
    const data = message.data as Record<string, unknown> | undefined;

    switch (message.type) {
      case 'attachFile':
        await this.handleAttachFile();
        break;

      case 'showContextPicker':
        await this.handleShowContextPicker();
        break;

      case 'getWorkspaceFiles':
        await this.handleGetWorkspaceFiles(data?.query as string | undefined);
        break;

      case 'openFile':
        await this.handleOpenFile(data?.path as string | undefined);
        break;

      case 'openDiff':
        await this.handleOpenDiff(data);
        break;

      default:
        console.warn(
          '[FileMessageHandler] Unknown message type:',
          message.type,
        );
        break;
    }
  }

  /**
   * Handle attach file request
   */
  private async handleAttachFile(): Promise<void> {
    try {
      const uris = await vscode.window.showOpenDialog({
        canSelectMany: false,
        canSelectFiles: true,
        canSelectFolders: false,
        openLabel: 'Attach',
      });

      if (uris && uris.length > 0) {
        const uri = uris[0];
        const fileName = getFileName(uri.fsPath);

        this.sendToWebView({
          type: 'fileAttached',
          data: {
            id: `file-${Date.now()}`,
            type: 'file',
            name: fileName,
            value: uri.fsPath,
          },
        });
      }
    } catch (error) {
      console.error('[FileMessageHandler] Failed to attach file:', error);
      this.sendToWebView({
        type: 'error',
        data: { message: `Failed to attach file: ${error}` },
      });
    }
  }

  /**
   * Handle show context picker request
   */
  private async handleShowContextPicker(): Promise<void> {
    try {
      const items: vscode.QuickPickItem[] = [];

      // Add current file
      const activeEditor = vscode.window.activeTextEditor;
      if (activeEditor) {
        const fileName = getFileName(activeEditor.document.uri.fsPath);
        items.push({
          label: `$(file) ${fileName}`,
          description: 'Current file',
          detail: activeEditor.document.uri.fsPath,
        });
      }

      // Add file picker option
      items.push({
        label: '$(file) File...',
        description: 'Choose a file to attach',
      });

      // Add workspace files option
      items.push({
        label: '$(search) Search files...',
        description: 'Search workspace files',
      });

      const selected = await vscode.window.showQuickPick(items, {
        placeHolder: 'Attach context',
        matchOnDescription: true,
        matchOnDetail: true,
      });

      if (selected) {
        if (selected.label.includes('Current file') && activeEditor) {
          const fileName = getFileName(activeEditor.document.uri.fsPath);
          this.sendToWebView({
            type: 'fileAttached',
            data: {
              id: `file-${Date.now()}`,
              type: 'file',
              name: fileName,
              value: activeEditor.document.uri.fsPath,
            },
          });
        } else if (selected.label.includes('File...')) {
          await this.handleAttachFile();
        } else if (selected.label.includes('Search files')) {
          const uri = await vscode.window.showOpenDialog({
            defaultUri: vscode.workspace.workspaceFolders?.[0]?.uri,
            canSelectMany: false,
            canSelectFiles: true,
            canSelectFolders: false,
            openLabel: 'Attach',
          });

          if (uri && uri.length > 0) {
            const fileName = getFileName(uri[0].fsPath);
            this.sendToWebView({
              type: 'fileAttached',
              data: {
                id: `file-${Date.now()}`,
                type: 'file',
                name: fileName,
                value: uri[0].fsPath,
              },
            });
          }
        }
      }
    } catch (error) {
      console.error(
        '[FileMessageHandler] Failed to show context picker:',
        error,
      );
      this.sendToWebView({
        type: 'error',
        data: { message: `Failed to show context picker: ${error}` },
      });
    }
  }

  /**
   * Get workspace files
   */
  private async handleGetWorkspaceFiles(query?: string): Promise<void> {
    try {
      const files: Array<{
        id: string;
        label: string;
        description: string;
        path: string;
      }> = [];
      const addedPaths = new Set<string>();

      const addFile = (uri: vscode.Uri, isCurrentFile = false) => {
        if (addedPaths.has(uri.fsPath)) {
          return;
        }

        const fileName = getFileName(uri.fsPath);
        const workspaceFolder = vscode.workspace.getWorkspaceFolder(uri);
        const relativePath = workspaceFolder
          ? vscode.workspace.asRelativePath(uri, false)
          : uri.fsPath;

        // Filter by query if provided
        if (
          query &&
          !fileName.toLowerCase().includes(query.toLowerCase()) &&
          !relativePath.toLowerCase().includes(query.toLowerCase())
        ) {
          return;
        }

        files.push({
          id: isCurrentFile ? 'current-file' : uri.fsPath,
          label: fileName,
          description: relativePath,
          path: uri.fsPath,
        });
        addedPaths.add(uri.fsPath);
      };

      // Search or show recent files
      if (query) {
        const uris = await vscode.workspace.findFiles(
          `**/*${query}*`,
          '**/node_modules/**',
          50,
        );

        for (const uri of uris) {
          addFile(uri);
        }
      } else {
        // Add current active file first
        const activeEditor = vscode.window.activeTextEditor;
        if (activeEditor) {
          addFile(activeEditor.document.uri, true);
        }

        // Add all open tabs
        const tabGroups = vscode.window.tabGroups.all;
        for (const tabGroup of tabGroups) {
          for (const tab of tabGroup.tabs) {
            const input = tab.input as { uri?: vscode.Uri } | undefined;
            if (input && input.uri instanceof vscode.Uri) {
              addFile(input.uri);
            }
          }
        }

        // If not enough files, add some workspace files
        if (files.length < 10) {
          const recentUris = await vscode.workspace.findFiles(
            '**/*',
            '**/node_modules/**',
            20,
          );

          for (const uri of recentUris) {
            if (files.length >= 20) {
              break;
            }
            addFile(uri);
          }
        }
      }

      this.sendToWebView({
        type: 'workspaceFiles',
        data: { files },
      });
    } catch (error) {
      console.error(
        '[FileMessageHandler] Failed to get workspace files:',
        error,
      );
      this.sendToWebView({
        type: 'error',
        data: { message: `Failed to get workspace files: ${error}` },
      });
    }
  }

  /**
   * Open file
   */
  private async handleOpenFile(path?: string): Promise<void> {
    if (!path) {
      console.warn('[FileMessageHandler] No path provided for openFile');
      return;
    }

    try {
      const uri = vscode.Uri.file(path);
      await vscode.window.showTextDocument(uri, {
        preview: false,
        preserveFocus: false,
      });
    } catch (error) {
      console.error('[FileMessageHandler] Failed to open file:', error);
      vscode.window.showErrorMessage(`Failed to open file: ${error}`);
    }
  }

  /**
   * Open diff view
   */
  private async handleOpenDiff(
    data: Record<string, unknown> | undefined,
  ): Promise<void> {
    if (!data) {
      console.warn('[FileMessageHandler] No data provided for openDiff');
      return;
    }

    try {
      await vscode.commands.executeCommand('qwenCode.showDiff', {
        path: (data.path as string) || '',
        oldText: (data.oldText as string) || '',
        newText: (data.newText as string) || '',
      });
    } catch (error) {
      console.error('[FileMessageHandler] Failed to open diff:', error);
      vscode.window.showErrorMessage(`Failed to open diff: ${error}`);
    }
  }
}
