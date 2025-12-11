/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * Creates a dev-only console logger for the WebView bundle.
 * In production builds it becomes a no-op to avoid noisy logs.
 */
export function createWebviewConsoleLogger(scope?: string) {
  return (...args: unknown[]) => {
    const env = (globalThis as { process?: { env?: Record<string, string> } })
      .process?.env;
    const isProduction = env?.NODE_ENV === 'production';
    if (isProduction) {
      return;
    }
    if (scope) {
      console.log(`[${scope}]`, ...args);
      return;
    }
    console.log(...args);
  };
}
