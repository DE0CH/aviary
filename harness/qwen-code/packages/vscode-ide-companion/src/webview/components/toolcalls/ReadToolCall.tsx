/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 *
 * Read tool call component - specialized for file reading operations
 */

import type React from 'react';
import type { BaseToolCallProps } from './shared/types.js';
import { ToolCallContainer } from './shared/LayoutComponents.js';
import { groupContent } from './shared/utils.js';

/**
 * Specialized component for Read tool calls
 * Optimized for displaying file reading operations
 * Shows: Read filename (no content preview)
 */
export const ReadToolCall: React.FC<BaseToolCallProps> = ({ toolCall }) => {
  const { content, locations, toolCallId } = toolCall;

  // Group content by type
  const { errors } = groupContent(content);

  // Extract filename from path
  const getFileName = (path: string): string => path.split('/').pop() || path;

  // Error case: show error
  if (errors.length > 0) {
    const path = locations?.[0]?.path || '';
    const fileName = path ? getFileName(path) : '';
    return (
      <ToolCallContainer
        label={fileName ? `Read ${fileName}` : 'Read'}
        status="error"
        toolCallId={toolCallId}
      >
        {errors.join('\n')}
      </ToolCallContainer>
    );
  }

  // Success case: show which file was read with filename in label
  if (locations && locations.length > 0) {
    const fileName = getFileName(locations[0].path);
    return (
      <ToolCallContainer
        label={`Read ${fileName}`}
        status="success"
        toolCallId={toolCallId}
      >
        {null}
      </ToolCallContainer>
    );
  }

  // No file info, don't show
  return null;
};
