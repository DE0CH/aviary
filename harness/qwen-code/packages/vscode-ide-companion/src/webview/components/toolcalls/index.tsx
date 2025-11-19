/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 *
 * Tool call component factory - routes to specialized components by kind
 */

import type React from 'react';
import type { BaseToolCallProps } from './shared/types.js';
import { shouldShowToolCall } from './shared/utils.js';
import { GenericToolCall } from './GenericToolCall.js';
import { ReadToolCall } from './ReadToolCall.js';
import { WriteToolCall } from './WriteToolCall.js';
import { ExecuteToolCall } from './ExecuteToolCall.js';
import { SearchToolCall } from './SearchToolCall.js';
import { ThinkToolCall } from './ThinkToolCall.js';

/**
 * Factory function that returns the appropriate tool call component based on kind
 */
export const getToolCallComponent = (
  kind: string,
): React.FC<BaseToolCallProps> => {
  const normalizedKind = kind.toLowerCase();

  // Route to specialized components
  switch (normalizedKind) {
    case 'read':
      return ReadToolCall;

    case 'write':
    case 'edit':
      return WriteToolCall;

    case 'execute':
    case 'bash':
    case 'command':
      return ExecuteToolCall;

    case 'search':
    case 'grep':
    case 'glob':
    case 'find':
      return SearchToolCall;

    case 'think':
    case 'thinking':
      return ThinkToolCall;

    // Add more specialized components as needed
    // case 'fetch':
    //   return FetchToolCall;
    // case 'delete':
    //   return DeleteToolCall;

    default:
      // Fallback to generic component
      return GenericToolCall;
  }
};

/**
 * Main tool call component that routes to specialized implementations
 */
export const ToolCallRouter: React.FC<BaseToolCallProps> = ({ toolCall }) => {
  // Check if we should show this tool call (hide internal ones)
  if (!shouldShowToolCall(toolCall.kind)) {
    return null;
  }

  // Get the appropriate component for this kind
  const Component = getToolCallComponent(toolCall.kind);

  // Render the specialized component
  return <Component toolCall={toolCall} />;
};

// Re-export types for convenience
export type { BaseToolCallProps, ToolCallData } from './shared/types.js';
