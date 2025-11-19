/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 *
 * Write/Edit tool call component - specialized for file writing and editing operations
 */

import type React from 'react';
import type { BaseToolCallProps } from './shared/types.js';
import {
  ToolCallCard,
  ToolCallRow,
  StatusIndicator,
  CodeBlock,
  LocationsList,
  DiffDisplay,
} from './shared/LayoutComponents.js';
import { formatValue, safeTitle, groupContent } from './shared/utils.js';

/**
 * Specialized component for Write/Edit tool calls
 * Optimized for displaying file writing and editing operations with diffs
 */
export const WriteToolCall: React.FC<BaseToolCallProps> = ({ toolCall }) => {
  const { kind, title, status, rawInput, content, locations } = toolCall;
  const titleText = safeTitle(title);
  const isEdit = kind.toLowerCase() === 'edit';

  // Group content by type
  const { textOutputs, errors, diffs, otherData } = groupContent(content);

  return (
    <ToolCallCard icon="✏️">
      {/* Title row */}
      <ToolCallRow label={isEdit ? 'Edit' : 'Write'}>
        <StatusIndicator status={status} text={titleText} />
      </ToolCallRow>

      {/* File path(s) */}
      {locations && locations.length > 0 && (
        <ToolCallRow label="File">
          <LocationsList locations={locations} />
        </ToolCallRow>
      )}

      {/* Input parameters (e.g., old_string, new_string for edits) */}
      {rawInput && (
        <ToolCallRow label="Changes">
          <CodeBlock>{formatValue(rawInput)}</CodeBlock>
        </ToolCallRow>
      )}

      {/* Diff display - most important for write/edit operations */}
      {diffs.map(
        (item: import('./shared/types.js').ToolCallContent, idx: number) => (
          <ToolCallRow key={`diff-${idx}`} label="Diff">
            <DiffDisplay
              path={item.path}
              oldText={item.oldText}
              newText={item.newText}
            />
          </ToolCallRow>
        ),
      )}

      {/* Success message or output */}
      {textOutputs.length > 0 && (
        <ToolCallRow label="Result">
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
