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
  LocationsList,
} from './shared/LayoutComponents.js';
import { DiffDisplay } from './shared/DiffDisplay.js';
import { groupContent } from './shared/utils.js';
import { useVSCode } from '../../hooks/useVSCode.js';

/**
 * Specialized component for Write/Edit tool calls
 * Optimized for displaying file writing and editing operations with diffs
 * Follows minimal display principle: only show what matters
 */
export const WriteToolCall: React.FC<BaseToolCallProps> = ({ toolCall }) => {
  const { kind, status: _status, content, locations } = toolCall;
  const isEdit = kind.toLowerCase() === 'edit';
  const vscode = useVSCode();

  // Group content by type
  const { errors, diffs } = groupContent(content);

  const handleOpenDiff = (
    path: string | undefined,
    oldText: string | null | undefined,
    newText: string | undefined,
  ) => {
    if (path) {
      vscode.postMessage({
        type: 'openDiff',
        data: { path, oldText: oldText || '', newText: newText || '' },
      });
    }
  };

  // Error case: show error with operation label
  if (errors.length > 0) {
    return (
      <ToolCallCard icon="✏️">
        <ToolCallRow label={isEdit ? 'Edit' : 'Write'}>
          <div style={{ color: '#c74e39', fontWeight: 500 }}>
            {errors.join('\n')}
          </div>
        </ToolCallRow>
      </ToolCallCard>
    );
  }

  // Success case with diff: show diff (already has file path)
  if (diffs.length > 0) {
    return (
      <ToolCallCard icon="✏️">
        {diffs.map(
          (item: import('./shared/types.js').ToolCallContent, idx: number) => (
            <div key={`diff-${idx}`} style={{ gridColumn: '1 / -1' }}>
              <DiffDisplay
                path={item.path}
                oldText={item.oldText}
                newText={item.newText}
                onOpenDiff={() =>
                  handleOpenDiff(item.path, item.oldText, item.newText)
                }
              />
            </div>
          ),
        )}
      </ToolCallCard>
    );
  }

  // Success case without diff: show operation + file
  if (locations && locations.length > 0) {
    return (
      <ToolCallCard icon="✏️">
        <ToolCallRow label={isEdit ? 'Edited' : 'Created'}>
          <LocationsList locations={locations} />
        </ToolCallRow>
      </ToolCallCard>
    );
  }

  // No output, don't show anything
  return null;
};
