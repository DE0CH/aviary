/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 *
 * Shared utility functions for tool call components
 */

import type { ToolCallContent, GroupedContent } from './types.js';

/**
 * Format any value to a string for display
 */
export const formatValue = (value: unknown): string => {
  if (value === null || value === undefined) {
    return '';
  }
  if (typeof value === 'string') {
    return value;
  }
  if (typeof value === 'object') {
    try {
      return JSON.stringify(value, null, 2);
    } catch (_e) {
      return String(value);
    }
  }
  return String(value);
};

/**
 * Safely convert title to string, handling object types
 */
export const safeTitle = (title: unknown): string => {
  if (typeof title === 'string') {
    return title;
  }
  if (title && typeof title === 'object') {
    return JSON.stringify(title);
  }
  return 'Tool Call';
};

/**
 * Get icon emoji for a given tool kind
 */
export const getKindIcon = (kind: string): string => {
  const kindMap: Record<string, string> = {
    edit: '✏️',
    write: '✏️',
    read: '📖',
    execute: '⚡',
    fetch: '🌐',
    delete: '🗑️',
    move: '📦',
    search: '🔍',
    think: '💭',
    diff: '📝',
  };
  return kindMap[kind.toLowerCase()] || '🔧';
};

/**
 * Check if a tool call should be displayed
 * Hides internal tool calls
 */
export const shouldShowToolCall = (kind: string): boolean =>
  !kind.includes('internal');

/**
 * Group tool call content by type to avoid duplicate labels
 */
export const groupContent = (content?: ToolCallContent[]): GroupedContent => {
  const textOutputs: string[] = [];
  const errors: string[] = [];
  const diffs: ToolCallContent[] = [];
  const otherData: unknown[] = [];

  content?.forEach((item) => {
    if (item.type === 'diff') {
      diffs.push(item);
    } else if (item.content) {
      const contentObj = item.content;

      // Handle error content
      if (contentObj.type === 'error' || 'error' in contentObj) {
        const errorMsg =
          formatValue(contentObj.error) ||
          formatValue(contentObj.text) ||
          'An error occurred';
        errors.push(errorMsg);
      }
      // Handle text content
      else if (contentObj.text) {
        textOutputs.push(formatValue(contentObj.text));
      }
      // Handle other content
      else {
        otherData.push(contentObj);
      }
    }
  });

  return { textOutputs, errors, diffs, otherData };
};
