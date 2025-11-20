/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 *
 * Execute tool call component - specialized for command execution operations
 */

import type React from 'react';
import type { BaseToolCallProps } from './shared/types.js';
import { ToolCallCard, ToolCallRow } from './shared/LayoutComponents.js';
import { safeTitle, groupContent } from './shared/utils.js';

/**
 * Specialized component for Execute tool calls
 * Optimized for displaying command execution with stdout/stderr
 * Shows command + output (if any) or error
 */
export const ExecuteToolCall: React.FC<BaseToolCallProps> = ({ toolCall }) => {
  const { title, content } = toolCall;
  const commandText = safeTitle(title);

  // Group content by type
  const { textOutputs, errors } = groupContent(content);

  // Error case: show command + error
  if (errors.length > 0) {
    return (
      <ToolCallCard icon="⚡">
        <ToolCallRow label="Command">
          <div style={{ fontFamily: 'var(--app-monospace-font-family)' }}>
            {commandText}
          </div>
        </ToolCallRow>
        <ToolCallRow label="Error">
          <div
            style={{
              color: '#c74e39',
              fontWeight: 500,
              whiteSpace: 'pre-wrap',
            }}
          >
            {errors.join('\n')}
          </div>
        </ToolCallRow>
      </ToolCallCard>
    );
  }

  // Success with output: show command + output (limited)
  if (textOutputs.length > 0) {
    const output = textOutputs.join('\n');
    const truncatedOutput =
      output.length > 500 ? output.substring(0, 500) + '...' : output;

    return (
      <ToolCallCard icon="⚡">
        <ToolCallRow label="Command">
          <div style={{ fontFamily: 'var(--app-monospace-font-family)' }}>
            {commandText}
          </div>
        </ToolCallRow>
        <ToolCallRow label="Output">
          <div
            style={{
              fontFamily: 'var(--app-monospace-font-family)',
              fontSize: '13px',
              whiteSpace: 'pre-wrap',
              opacity: 0.9,
            }}
          >
            {truncatedOutput}
          </div>
        </ToolCallRow>
      </ToolCallCard>
    );
  }

  // Success without output: show command only
  return (
    <ToolCallCard icon="⚡">
      <ToolCallRow label="Executed">
        <div style={{ fontFamily: 'var(--app-monospace-font-family)' }}>
          {commandText}
        </div>
      </ToolCallRow>
    </ToolCallCard>
  );
};
