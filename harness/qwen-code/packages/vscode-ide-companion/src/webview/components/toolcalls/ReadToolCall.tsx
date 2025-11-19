/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 *
 * Read tool call component - specialized for file reading operations
 */

import type React from 'react';
import type { BaseToolCallProps } from './shared/types.js';
import {
  ToolCallCard,
  ToolCallRow,
  StatusIndicator,
  CodeBlock,
  LocationsList,
} from './shared/LayoutComponents.js';
import { formatValue, safeTitle, groupContent } from './shared/utils.js';

/**
 * Specialized component for Read tool calls
 * Optimized for displaying file reading operations
 */
export const ReadToolCall: React.FC<BaseToolCallProps> = ({ toolCall }) => {
  const { title, status, rawInput, content, locations } = toolCall;
  const titleText = safeTitle(title);

  // Group content by type
  const { textOutputs, errors, otherData } = groupContent(content);

  return (
    <ToolCallCard icon="📖">
      {/* Title row */}
      <ToolCallRow label="Read">
        <StatusIndicator status={status} text={titleText} />
      </ToolCallRow>

      {/* File path(s) */}
      {locations && locations.length > 0 && (
        <ToolCallRow label="File">
          <LocationsList locations={locations} />
        </ToolCallRow>
      )}

      {/* Input parameters (e.g., line range, offset) */}
      {rawInput && (
        <ToolCallRow label="Options">
          <CodeBlock>{formatValue(rawInput)}</CodeBlock>
        </ToolCallRow>
      )}

      {/* File content output */}
      {textOutputs.length > 0 && (
        <ToolCallRow label="Content">
          <CodeBlock>{textOutputs.join('\n')}</CodeBlock>
        </ToolCallRow>
      )}

      {/* Error handling */}
      {errors.length > 0 && (
        <ToolCallRow label="Error">
          <div style={{ color: '#c74e39' }}>{errors.join('\n')}</div>
        </ToolCallRow>
      )}

      {/* Other data */}
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
