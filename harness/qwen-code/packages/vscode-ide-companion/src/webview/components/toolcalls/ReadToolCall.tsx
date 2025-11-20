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
  LocationsList,
} from './shared/LayoutComponents.js';
import { groupContent } from './shared/utils.js';

/**
 * Specialized component for Read tool calls
 * Optimized for displaying file reading operations
 * Minimal display: just show file name, hide content (too verbose)
 */
export const ReadToolCall: React.FC<BaseToolCallProps> = ({ toolCall }) => {
  const { content, locations } = toolCall;

  // Group content by type
  const { errors } = groupContent(content);

  // Error case: show error with operation label
  if (errors.length > 0) {
    return (
      <ToolCallCard icon="📖">
        <ToolCallRow label="Read">
          <div style={{ color: '#c74e39', fontWeight: 500 }}>
            {errors.join('\n')}
          </div>
        </ToolCallRow>
      </ToolCallCard>
    );
  }

  // Success case: show which file was read
  if (locations && locations.length > 0) {
    return (
      <ToolCallCard icon="📖">
        <ToolCallRow label="Read">
          <LocationsList locations={locations} />
        </ToolCallRow>
      </ToolCallCard>
    );
  }

  // No file info, don't show
  return null;
};
