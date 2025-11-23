/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 *
 * Edit tool call component - specialized for file editing operations
 */

import type React from 'react';
import { useState } from 'react';
import type { BaseToolCallProps } from './shared/types.js';
import { ToolCallContainer } from './shared/LayoutComponents.js';
import { DiffDisplay } from './shared/DiffDisplay.js';
import { groupContent } from './shared/utils.js';
import { useVSCode } from '../../hooks/useVSCode.js';
import { FileLink } from '../shared/FileLink.js';

/**
 * Calculate diff summary (added/removed lines)
 */
const getDiffSummary = (
  oldText: string | null | undefined,
  newText: string | undefined,
): string => {
  const oldLines = oldText ? oldText.split('\n').length : 0;
  const newLines = newText ? newText.split('\n').length : 0;
  const diff = newLines - oldLines;

  if (diff > 0) {
    return `+${diff} lines`;
  } else if (diff < 0) {
    return `${diff} lines`;
  } else {
    return 'Modified';
  }
};

/**
 * Specialized component for Edit tool calls
 * Optimized for displaying file editing operations with diffs
 */
export const EditToolCall: React.FC<BaseToolCallProps> = ({ toolCall }) => {
  const { content, locations, toolCallId } = toolCall;
  const vscode = useVSCode();
  const [expanded, setExpanded] = useState(false);

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

  // Extract filename from path
  const getFileName = (path: string): string => path.split('/').pop() || path;

  // Error case: show error
  if (errors.length > 0) {
    const path = diffs[0]?.path || locations?.[0]?.path || '';
    const fileName = path ? getFileName(path) : '';
    return (
      <ToolCallContainer
        label={fileName ? `Edit ${fileName}` : 'Edit'}
        status="error"
        toolCallId={toolCallId}
      >
        {errors.join('\n')}
      </ToolCallContainer>
    );
  }

  // Success case with diff: show collapsible format
  if (diffs.length > 0) {
    const firstDiff = diffs[0];
    const path = firstDiff.path || (locations && locations[0]?.path) || '';
    const fileName = path ? getFileName(path) : '';
    const summary = getDiffSummary(firstDiff.oldText, firstDiff.newText);

    return (
      <div>
        <div
          className="relative py-2 select-text cursor-pointer hover:bg-[var(--app-input-background)]"
          onClick={() => setExpanded(!expanded)}
        >
          <span className="absolute left-2 top-[10px] text-[10px] text-[#74c991]">
            ●
          </span>
          <div className="flex flex-col gap-1 pl-[30px]">
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-2">
                <span className="text-[13px] font-medium text-[var(--app-primary-foreground)]">
                  Edit {fileName}
                </span>
                {toolCallId && (
                  <span className="text-[10px] opacity-30">
                    [{toolCallId.slice(-8)}]
                  </span>
                )}
              </div>
              <span className="text-xs opacity-60 mr-2">
                {expanded ? '▼' : '▶'}
              </span>
            </div>
            <div className="inline-flex text-[var(--app-secondary-foreground)] text-[0.85em] opacity-70 flex-row items-start w-full gap-1">
              <span className="flex-shrink-0 relative top-[-0.1em]">⎿</span>
              <span className="flex-shrink-0 w-full">{summary}</span>
            </div>
          </div>
        </div>
        {expanded && (
          <div className="ml-[30px] mt-1">
            {diffs.map(
              (
                item: import('./shared/types.js').ToolCallContent,
                idx: number,
              ) => (
                <DiffDisplay
                  key={`diff-${idx}`}
                  path={item.path}
                  oldText={item.oldText}
                  newText={item.newText}
                  onOpenDiff={() =>
                    handleOpenDiff(item.path, item.oldText, item.newText)
                  }
                />
              ),
            )}
          </div>
        )}
      </div>
    );
  }

  // Success case without diff: show file in compact format
  if (locations && locations.length > 0) {
    const fileName = getFileName(locations[0].path);
    return (
      <ToolCallContainer
        label={`Edited ${fileName}`}
        status="success"
        toolCallId={toolCallId}
      >
        <div className="inline-flex text-[var(--app-secondary-foreground)] text-[0.85em] opacity-70 flex-row items-start w-full gap-1">
          <span className="flex-shrink-0 relative top-[-0.1em]">⎿</span>
          <FileLink
            path={locations[0].path}
            line={locations[0].line}
            showFullPath={true}
          />
        </div>
      </ToolCallContainer>
    );
  }

  // No output, don't show anything
  return null;
};
