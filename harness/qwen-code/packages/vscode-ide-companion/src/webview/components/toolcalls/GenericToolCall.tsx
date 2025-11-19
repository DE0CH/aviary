/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 *
 * Generic tool call component - handles all tool call types as fallback
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
import {
  formatValue,
  safeTitle,
  getKindIcon,
  groupContent,
} from './shared/utils.js';

/**
 * Generic tool call component that can display any tool call type
 * Used as fallback for unknown tool call kinds
 */
export const GenericToolCall: React.FC<BaseToolCallProps> = ({ toolCall }) => {
  const { kind, title, status, rawInput, content, locations } = toolCall;
  const kindIcon = getKindIcon(kind);
  const titleText = safeTitle(title);

  // Group content by type
  const { textOutputs, errors, diffs, otherData } = groupContent(content);

  return (
    <ToolCallCard icon={kindIcon}>
      {/* Title row */}
      <ToolCallRow label="Tool">
        <StatusIndicator status={status} text={titleText} />
      </ToolCallRow>

      {/* Input row */}
      {rawInput && (
        <ToolCallRow label="Input">
          <CodeBlock>{formatValue(rawInput)}</CodeBlock>
        </ToolCallRow>
      )}

      {/* Locations row */}
      {locations && locations.length > 0 && (
        <ToolCallRow label="Files">
          <LocationsList locations={locations} />
        </ToolCallRow>
      )}

      {/* Output row - combined text outputs */}
      {textOutputs.length > 0 && (
        <ToolCallRow label="Output">
          <CodeBlock>{textOutputs.join('\n')}</CodeBlock>
        </ToolCallRow>
      )}

      {/* Error row - combined errors */}
      {errors.length > 0 && (
        <ToolCallRow label="Error">
          <div style={{ color: '#c74e39' }}>{errors.join('\n')}</div>
        </ToolCallRow>
      )}

      {/* Diff rows */}
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

      {/* Other data rows */}
      {otherData.length > 0 && (
        <ToolCallRow label="Data">
          <CodeBlock>
            {otherData.map((data: unknown) => formatValue(data)).join('\n\n')}
          </CodeBlock>
        </ToolCallRow>
      )}
    </ToolCallCard>
  );
};
