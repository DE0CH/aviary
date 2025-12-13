/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

const AUTH_ERROR_PATTERNS = [
  'Authentication required',
  '(code: -32000)',
  'Unauthorized',
  'Invalid token',
  'Session expired',
];

export const isAuthenticationRequiredError = (error: unknown): boolean => {
  if (!error) {
    return false;
  }
  const message =
    error instanceof Error
      ? error.message
      : typeof error === 'string'
        ? error
        : String(error);
  return AUTH_ERROR_PATTERNS.some((pattern) => message.includes(pattern));
};
