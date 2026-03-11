/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

export function getErrorMessage(
  error: unknown,
  fallback = 'Unknown error',
): string {
  const extractDetailsMessage = (value: unknown): string | null => {
    if (typeof value === 'string' && value) {
      return value;
    }

    if (typeof value !== 'object' || value === null) {
      return null;
    }

    const record = value as Record<string, unknown>;
    const details = record['details'];
    if (typeof details === 'string' && details) {
      return details;
    }
    if (typeof details === 'object' && details !== null) {
      const detailsRecord = details as Record<string, unknown>;
      if (
        typeof detailsRecord['message'] === 'string' &&
        detailsRecord['message']
      ) {
        return detailsRecord['message'];
      }
      try {
        return JSON.stringify(details);
      } catch {
        return null;
      }
    }
    return null;
  };

  if (error instanceof Error && error.message) {
    return error.message;
  }
  if (typeof error === 'string' && error) {
    return error;
  }
  if (typeof error === 'object' && error !== null) {
    const record = error as Record<string, unknown>;
    if (typeof record['message'] === 'string' && record['message']) {
      return record['message'];
    }
    const topLevelDetailsMessage = extractDetailsMessage(record['data']);
    if (topLevelDetailsMessage) {
      return topLevelDetailsMessage;
    }
    const nested = record['error'];
    if (typeof nested === 'object' && nested !== null) {
      const nestedRecord = nested as Record<string, unknown>;
      const nestedMessage = nestedRecord['message'];
      if (typeof nestedMessage === 'string' && nestedMessage) {
        return nestedMessage;
      }
      const nestedDetailsMessage = extractDetailsMessage(nestedRecord['data']);
      if (nestedDetailsMessage) {
        return nestedDetailsMessage;
      }
    }
    try {
      return JSON.stringify(error);
    } catch {
      return fallback;
    }
  }
  return fallback;
}
