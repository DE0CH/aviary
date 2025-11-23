/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 *
 * TodoWrite tool call component - specialized for todo list operations
 */

import type React from 'react';
import type { BaseToolCallProps } from './shared/types.js';
import { ToolCallContainer } from './shared/LayoutComponents.js';
import { groupContent } from './shared/utils.js';

/**
 * Specialized component for TodoWrite tool calls
 * Optimized for displaying todo list update operations
 */
export const TodoWriteToolCall: React.FC<BaseToolCallProps> = ({
  toolCall,
}) => {
  const { content } = toolCall;

  // Group content by type
  const { errors, textOutputs } = groupContent(content);

  // Error case: show error
  if (errors.length > 0) {
    return (
      <ToolCallContainer label="Update Todos" status="error">
        {errors.join('\n')}
      </ToolCallContainer>
    );
  }

  // Success case: show simple confirmation
  const outputText =
    textOutputs.length > 0 ? textOutputs.join(' ') : 'Todos updated';

  // Truncate if too long
  const displayText =
    outputText.length > 100 ? outputText.substring(0, 100) + '...' : outputText;

  return (
    <ToolCallContainer label="Update Todos" status="success">
      {displayText}
    </ToolCallContainer>
  );
};
