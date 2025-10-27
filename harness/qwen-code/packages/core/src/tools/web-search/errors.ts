/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * Custom error class for web search operations.
 */
export class WebSearchError extends Error {
  constructor(
    readonly provider: string,
    message: string,
    readonly originalError?: unknown,
  ) {
    super(`[${provider}] ${message}`);
    this.name = 'WebSearchError';
  }
}
