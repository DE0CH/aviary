/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

// Use explicit Vitest imports instead of relying on globals.
import { describe, it, expect } from 'vitest';
import type { ToolCallData } from '../toolcalls/shared/types.js';
import { hasToolCallOutput } from '../toolcalls/shared/utils.js';

describe('Message Ordering', () => {
  it('should correctly identify tool calls with output', () => {
    // Test failed tool call (should show)
    const failedToolCall: ToolCallData = {
      toolCallId: 'test-1',
      kind: 'read',
      title: 'Read file',
      status: 'failed',
      timestamp: 1000,
    };
    expect(hasToolCallOutput(failedToolCall)).toBe(true);

    // Test execute tool call with title (should show)
    const executeToolCall: ToolCallData = {
      toolCallId: 'test-2',
      kind: 'execute',
      title: 'ls -la',
      status: 'completed',
      timestamp: 2000,
    };
    expect(hasToolCallOutput(executeToolCall)).toBe(true);

    // Test tool call with content (should show)
    const contentToolCall: ToolCallData = {
      toolCallId: 'test-3',
      kind: 'read',
      title: 'Read file',
      status: 'completed',
      content: [
        {
          type: 'content',
          content: {
            type: 'text',
            text: 'File content',
          },
        },
      ],
      timestamp: 3000,
    };
    expect(hasToolCallOutput(contentToolCall)).toBe(true);

    // Test tool call with locations (should show)
    const locationToolCall: ToolCallData = {
      toolCallId: 'test-4',
      kind: 'read',
      title: 'Read file',
      status: 'completed',
      locations: [
        {
          path: '/path/to/file.txt',
        },
      ],
      timestamp: 4000,
    };
    expect(hasToolCallOutput(locationToolCall)).toBe(true);

    // Test tool call with title (should show)
    const titleToolCall: ToolCallData = {
      toolCallId: 'test-5',
      kind: 'generic',
      title: 'Generic tool call',
      status: 'completed',
      timestamp: 5000,
    };
    expect(hasToolCallOutput(titleToolCall)).toBe(true);

    // Test tool call without output (should not show)
    const noOutputToolCall: ToolCallData = {
      toolCallId: 'test-6',
      kind: 'generic',
      title: '',
      status: 'completed',
      timestamp: 6000,
    };
    expect(hasToolCallOutput(noOutputToolCall)).toBe(false);
  });
});
