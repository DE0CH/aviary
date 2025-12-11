/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import * as vscode from 'vscode';

type ConsoleLogger = (...args: unknown[]) => void;

// Shared console logger instance, initialized during extension activation.
let sharedConsoleLogger: ConsoleLogger = () => {};

export function createLogger(
  context: vscode.ExtensionContext,
  logger: vscode.OutputChannel,
) {
  return (message: string) => {
    if (context.extensionMode === vscode.ExtensionMode.Development) {
      logger.appendLine(message);
    }
  };
}

/**
 * Creates a dev-only logger that writes to the VS Code console (Developer Tools).
 */
export function createConsoleLogger(
  context: vscode.ExtensionContext,
  scope?: string,
): ConsoleLogger {
  return (...args: unknown[]) => {
    if (context.extensionMode !== vscode.ExtensionMode.Development) {
      return;
    }
    if (scope) {
      console.log(`[${scope}]`, ...args);
      return;
    }
    console.log(...args);
  };
}

/**
 * Initialize the shared console logger so other modules can import it without
 * threading the extension context everywhere.
 */
export function initSharedConsoleLogger(
  context: vscode.ExtensionContext,
  scope?: string,
) {
  sharedConsoleLogger = createConsoleLogger(context, scope);
}

/**
 * Get the shared console logger (no-op until initialized).
 */
export function getConsoleLogger(): ConsoleLogger {
  return sharedConsoleLogger;
}
