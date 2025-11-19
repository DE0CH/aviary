/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 *
 * Execute tool call component - specialized for command execution operations
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
 * Specialized component for Execute tool calls
 * Optimized for displaying command execution with stdout/stderr
 */
export const ExecuteToolCall: React.FC<BaseToolCallProps> = ({ toolCall }) => {
  const { title, status, rawInput, content } = toolCall;
  const titleText = safeTitle(title);

  // Group content by type
  const { textOutputs, errors, otherData } = groupContent(content);

  return (
    <ToolCallCard icon="⚡">
      {/* Title row */}
      <ToolCallRow label="Execute">
        <StatusIndicator status={status} text={titleText} />
      </ToolCallRow>

      {/* Command */}
      {rawInput && (
        <ToolCallRow label="Command">
          <CodeBlock>{formatValue(rawInput)}</CodeBlock>
        </ToolCallRow>
      )}

      {/* Standard output */}
      {textOutputs.length > 0 && (
        <ToolCallRow label="Output">
          <CodeBlock>{textOutputs.join('\n')}</CodeBlock>
        </ToolCallRow>
      )}

      {/* Standard error / Errors */}
      {errors.length > 0 && (
        <ToolCallRow label="Error">
          <div style={{ color: '#c74e39' }}>
            <CodeBlock>{errors.join('\n')}</CodeBlock>
          </div>
        </ToolCallRow>
      )}

      {/* Exit code or other execution details */}
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
