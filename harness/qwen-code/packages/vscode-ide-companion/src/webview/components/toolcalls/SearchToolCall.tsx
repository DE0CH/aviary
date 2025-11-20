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
  LocationsList,
} from './shared/LayoutComponents.js';
import { safeTitle, groupContent } from './shared/utils.js';

/**
 * Specialized component for Search tool calls
 * Optimized for displaying search operations and results
 * Shows query + result count or file list
 */
export const SearchToolCall: React.FC<BaseToolCallProps> = ({ toolCall }) => {
  const { title, content, locations } = toolCall;
  const queryText = safeTitle(title);

  // Group content by type
  const { errors } = groupContent(content);

  // Error case: show search query + error
  if (errors.length > 0) {
    return (
      <ToolCallCard icon="🔍">
        <ToolCallRow label="Search">
          <div style={{ fontFamily: 'var(--app-monospace-font-family)' }}>
            {queryText}
          </div>
        </ToolCallRow>
        <ToolCallRow label="Error">
          <div style={{ color: '#c74e39', fontWeight: 500 }}>
            {errors.join('\n')}
          </div>
        </ToolCallRow>
      </ToolCallCard>
    );
  }

  // Success case with results: show search query + file list
  if (locations && locations.length > 0) {
    return (
      <ToolCallCard icon="🔍">
        <ToolCallRow label="Search">
          <div style={{ fontFamily: 'var(--app-monospace-font-family)' }}>
            {queryText}
          </div>
        </ToolCallRow>
        <ToolCallRow label={`Found (${locations.length})`}>
          <LocationsList locations={locations} />
        </ToolCallRow>
      </ToolCallCard>
    );
  }

  // No results
  return null;
};
