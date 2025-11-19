/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 *
 * Search tool call component - specialized for search operations
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
 * Specialized component for Search tool calls
 * Optimized for displaying search operations and results
 */
export const SearchToolCall: React.FC<BaseToolCallProps> = ({ toolCall }) => {
  const { title, status, rawInput, content, locations } = toolCall;
  const titleText = safeTitle(title);

  // Group content by type
  const { textOutputs, errors, otherData } = groupContent(content);

  return (
    <ToolCallCard icon="🔍">
      {/* Title row */}
      <ToolCallRow label="Search">
        <StatusIndicator status={status} text={titleText} />
      </ToolCallRow>

      {/* Search query/pattern */}
      {rawInput && (
        <ToolCallRow label="Query">
          <CodeBlock>{formatValue(rawInput)}</CodeBlock>
        </ToolCallRow>
      )}

      {/* Search results - files found */}
      {locations && locations.length > 0 && (
        <ToolCallRow label="Results">
          <LocationsList locations={locations} />
        </ToolCallRow>
      )}

      {/* Search output details */}
      {textOutputs.length > 0 && (
        <ToolCallRow label="Matches">
          <CodeBlock>{textOutputs.join('\n')}</CodeBlock>
        </ToolCallRow>
      )}

      {/* Error handling */}
      {errors.length > 0 && (
        <ToolCallRow label="Error">
          <div style={{ color: '#c74e39' }}>{errors.join('\n')}</div>
        </ToolCallRow>
      )}

      {/* Other search metadata */}
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
