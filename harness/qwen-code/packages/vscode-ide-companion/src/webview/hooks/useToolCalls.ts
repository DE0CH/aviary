/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import { useState, useCallback } from 'react';
import type { ToolCallData } from '../components/ToolCall.js';
import type { ToolCallUpdate } from '../types/toolCall.js';

/**
 * Tool call management Hook
 * Manages tool call states and updates
 */
export const useToolCalls = () => {
  const [toolCalls, setToolCalls] = useState<Map<string, ToolCallData>>(
    new Map(),
  );

  /**
   * Handle tool call update
   */
  const handleToolCallUpdate = useCallback((update: ToolCallUpdate) => {
    setToolCalls((prevToolCalls) => {
      const newMap = new Map(prevToolCalls);
      const existing = newMap.get(update.toolCallId);

      const safeTitle = (title: unknown): string => {
        if (typeof title === 'string') {
          return title;
        }
        if (title && typeof title === 'object') {
          return JSON.stringify(title);
        }
        return 'Tool Call';
      };

      if (update.type === 'tool_call') {
        const content = update.content?.map((item) => ({
          type: item.type as 'content' | 'diff',
          content: item.content,
          path: item.path,
          oldText: item.oldText,
          newText: item.newText,
        }));

        newMap.set(update.toolCallId, {
          toolCallId: update.toolCallId,
          kind: update.kind || 'other',
          title: safeTitle(update.title),
          status: update.status || 'pending',
          rawInput: update.rawInput as string | object | undefined,
          content,
          locations: update.locations,
        });
      } else if (update.type === 'tool_call_update') {
        const updatedContent = update.content
          ? update.content.map((item) => ({
              type: item.type as 'content' | 'diff',
              content: item.content,
              path: item.path,
              oldText: item.oldText,
              newText: item.newText,
            }))
          : undefined;

        if (existing) {
          const mergedContent = updatedContent
            ? [...(existing.content || []), ...updatedContent]
            : existing.content;

          newMap.set(update.toolCallId, {
            ...existing,
            ...(update.kind && { kind: update.kind }),
            ...(update.title && { title: safeTitle(update.title) }),
            ...(update.status && { status: update.status }),
            content: mergedContent,
            ...(update.locations && { locations: update.locations }),
          });
        } else {
          newMap.set(update.toolCallId, {
            toolCallId: update.toolCallId,
            kind: update.kind || 'other',
            title: update.title ? safeTitle(update.title) : '',
            status: update.status || 'pending',
            rawInput: update.rawInput as string | object | undefined,
            content: updatedContent,
            locations: update.locations,
          });
        }
      }

      return newMap;
    });
  }, []);

  /**
   * Clear all tool calls
   */
  const clearToolCalls = useCallback(() => {
    setToolCalls(new Map());
  }, []);

  /**
   * Get in-progress tool calls
   */
  const inProgressToolCalls = Array.from(toolCalls.values()).filter(
    (toolCall) =>
      toolCall.status === 'pending' || toolCall.status === 'in_progress',
  );

  /**
   * Get completed tool calls
   */
  const completedToolCalls = Array.from(toolCalls.values()).filter(
    (toolCall) =>
      toolCall.status === 'completed' || toolCall.status === 'failed',
  );

  return {
    toolCalls,
    inProgressToolCalls,
    completedToolCalls,
    handleToolCallUpdate,
    clearToolCalls,
  };
};
