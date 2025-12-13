/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * Authentication Error Utility
 *
 * Used to uniformly identify and handle various authentication-related error messages.
 * Determines if re-authentication is needed by matching predefined error patterns.
 *
 * @param error - The error object or string to check
 * @returns true if it's an authentication-related error, false otherwise
 */
const AUTH_ERROR_PATTERNS = [
  'Authentication required', // Standard authentication request message
  '(code: -32000)', // RPC error code -32000 indicates authentication failure
  'Unauthorized', // HTTP unauthorized error
  'Invalid token', // Invalid token
  'Session expired', // Session expired
];

/**
 * Determines if the given error is authentication-related
 *
 * This function detects various forms of authentication errors, including:
 * - Direct error objects
 * - String-form error messages
 * - Other types of errors converted to strings for pattern matching
 *
 * @param error - The error object to check, can be an Error instance, string, or other type
 * @returns boolean - true if the error is authentication-related, false otherwise
 */
export const isAuthenticationRequiredError = (error: unknown): boolean => {
  // Null check to avoid unnecessary processing
  if (!error) {
    return false;
  }

  // Extract error message text
  const message =
    error instanceof Error
      ? error.message
      : typeof error === 'string'
        ? error
        : String(error);

  // Match authentication-related errors using predefined patterns
  return AUTH_ERROR_PATTERNS.some((pattern) => message.includes(pattern));
};
