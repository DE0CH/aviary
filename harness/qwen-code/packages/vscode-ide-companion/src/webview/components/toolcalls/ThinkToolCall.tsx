/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 *
 * Think tool call component - specialized for thinking/reasoning operations
 */

import type React from 'react';
import type { BaseToolCallProps } from './shared/types.js';
import {
  ToolCallCard,
  ToolCallRow,
  StatusIndicator,
  CodeBlock,
} from './shared/LayoutComponents.js';
import { formatValue, safeTitle, groupContent } from './shared/utils.js';

/**
 * Specialized component for Think tool calls
 * Optimized for displaying AI reasoning and thought processes
 */
export const ThinkToolCall: React.FC<BaseToolCallProps> = ({ toolCall }) => {
  const { title, status, rawInput, content } = toolCall;
  const titleText = safeTitle(title);

  // Group content by type
  const { textOutputs, errors, otherData } = groupContent(content);

  return (
    <ToolCallCard icon="💭">
      {/* Title row */}
      <ToolCallRow label="Thinking">
        <StatusIndicator status={status} text={titleText} />
      </ToolCallRow>

      {/* Thinking context/prompt */}
      {rawInput && (
        <ToolCallRow label="Context">
          <CodeBlock>{formatValue(rawInput)}</CodeBlock>
        </ToolCallRow>
      )}

      {/* Thought content */}
      {textOutputs.length > 0 && (
        <ToolCallRow label="Thoughts">
          <div style={{ fontStyle: 'italic', opacity: 0.95 }}>
            {textOutputs.join('\n\n')}
          </div>
        </ToolCallRow>
      )}

      {/* Error handling */}
      {errors.length > 0 && (
        <ToolCallRow label="Error">
          <div style={{ color: '#c74e39' }}>{errors.join('\n')}</div>
        </ToolCallRow>
      )}

      {/* Other reasoning data */}
      {otherData.length > 0 && (
        <ToolCallRow label="Details">
          <CodeBlock>
            {otherData.map((data: unknown) => formatValue(data)).join('\n\n')}
          </CodeBlock>
        </ToolCallRow>
      )}
    </ToolCallCard>
  );
};
