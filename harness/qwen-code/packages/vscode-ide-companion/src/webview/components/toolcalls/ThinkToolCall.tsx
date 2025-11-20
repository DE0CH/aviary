/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 *
 * Think tool call component - specialized for thinking/reasoning operations
 */

import type React from 'react';
import type { BaseToolCallProps } from './shared/types.js';
import { ToolCallCard, ToolCallRow } from './shared/LayoutComponents.js';
import { groupContent } from './shared/utils.js';

/**
 * Specialized component for Think tool calls
 * Optimized for displaying AI reasoning and thought processes
 * Minimal display: just show the thoughts (no context)
 */
export const ThinkToolCall: React.FC<BaseToolCallProps> = ({ toolCall }) => {
  const { content } = toolCall;

  // Group content by type
  const { textOutputs, errors } = groupContent(content);

  // Error case (rare for thinking)
  if (errors.length > 0) {
    return (
      <ToolCallCard icon="💭">
        <ToolCallRow label="Error">
          <div style={{ color: '#c74e39', fontWeight: 500 }}>
            {errors.join('\n')}
          </div>
        </ToolCallRow>
      </ToolCallCard>
    );
  }

  // Show thoughts with label
  if (textOutputs.length > 0) {
    const thoughts = textOutputs.join('\n\n');
    const truncatedThoughts =
      thoughts.length > 500 ? thoughts.substring(0, 500) + '...' : thoughts;

    return (
      <ToolCallCard icon="💭">
        <ToolCallRow label="Thinking">
          <div
            style={{
              fontStyle: 'italic',
              opacity: 0.9,
              lineHeight: 1.6,
            }}
          >
            {truncatedThoughts}
          </div>
        </ToolCallRow>
      </ToolCallCard>
    );
  }

  // Empty thoughts
  return null;
};
