/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import { renderHook, act } from '@testing-library/react';
import { useToolCalls } from './useToolCalls';
import type { ToolCallUpdate } from '../types/toolCall.js';

describe('useToolCalls', () => {
  it('should add timestamp when creating tool call', () => {
    const { result } = renderHook(() => useToolCalls());

    const toolCallUpdate: ToolCallUpdate = {
      type: 'tool_call',
      toolCallId: 'test-1',
      kind: 'read',
      title: 'Read file',
      status: 'pending',
    };

    act(() => {
      result.current.handleToolCallUpdate(toolCallUpdate);
    });

    const toolCalls = Array.from(result.current.toolCalls.values());
    expect(toolCalls).toHaveLength(1);
    expect(toolCalls[0].timestamp).toBeDefined();
    expect(typeof toolCalls[0].timestamp).toBe('number');
  });

  it('should preserve timestamp when updating tool call', () => {
    const { result } = renderHook(() => useToolCalls());

    const timestamp = Date.now() - 1000; // 1 second ago

    // Create tool call with specific timestamp
    const toolCallUpdate: ToolCallUpdate = {
      type: 'tool_call',
      toolCallId: 'test-1',
      kind: 'read',
      title: 'Read file',
      status: 'pending',
      timestamp,
    };

    act(() => {
      result.current.handleToolCallUpdate(toolCallUpdate);
    });

    // Update tool call without timestamp
    const toolCallUpdate2: ToolCallUpdate = {
      type: 'tool_call_update',
      toolCallId: 'test-1',
      status: 'completed',
    };

    act(() => {
      result.current.handleToolCallUpdate(toolCallUpdate2);
    });

    const toolCalls = Array.from(result.current.toolCalls.values());
    expect(toolCalls).toHaveLength(1);
    expect(toolCalls[0].timestamp).toBe(timestamp);
  });

  it('should use current time as default timestamp', () => {
    const { result } = renderHook(() => useToolCalls());

    const before = Date.now();

    const toolCallUpdate: ToolCallUpdate = {
      type: 'tool_call',
      toolCallId: 'test-1',
      kind: 'read',
      title: 'Read file',
      status: 'pending',
      // No timestamp provided
    };

    act(() => {
      result.current.handleToolCallUpdate(toolCallUpdate);
    });

    const after = Date.now();

    const toolCalls = Array.from(result.current.toolCalls.values());
    expect(toolCalls).toHaveLength(1);
    expect(toolCalls[0].timestamp).toBeGreaterThanOrEqual(before);
    expect(toolCalls[0].timestamp).toBeLessThanOrEqual(after);
  });
});
