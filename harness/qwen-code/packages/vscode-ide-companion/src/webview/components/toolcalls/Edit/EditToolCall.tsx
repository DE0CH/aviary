/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 *
 * Edit tool call component - specialized for file editing operations
 */

import { useEffect, useCallback, useMemo } from 'react';
import type { BaseToolCallProps } from '../shared/types.js';
import { ToolCallContainer } from '../shared/LayoutComponents.js';
import {
  groupContent,
  mapToolStatusToContainerStatus,
} from '../shared/utils.js';
import { useVSCode } from '../../../hooks/useVSCode.js';
import { FileLink } from '../../ui/FileLink.js';
import { isDevelopmentMode } from '../../../utils/envUtils.js';

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

  // Group content by type; memoize to avoid new array identities on every render
  const { errors, diffs } = useMemo(() => groupContent(content), [content]);
  // TODO:
  // console.log('EditToolCall', {
  //   content,
  //   locations,
  //   toolCallId,
  //   errors,
  //   diffs,
  // });
  const handleOpenDiff = useCallback(
    (
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
    },
    [vscode],
  );

  // Extract filename from path
  const getFileName = (path: string): string => path.split('/').pop() || path;

  // Keep a module-scoped set to ensure auto-open fires once per toolCallId across re-renders
  // const autoOpenedToolCallIds =
  //   (
  //     globalThis as unknown as {
  //       __qwenAutoOpenedDiffIds?: Set<string>;
  //     }
  //   ).__qwenAutoOpenedDiffIds || new Set<string>();
  // (
  //   globalThis as unknown as { __qwenAutoOpenedDiffIds: Set<string> }
  // ).__qwenAutoOpenedDiffIds = autoOpenedToolCallIds;

  // Automatically trigger openDiff when diff content is detected (Claude Code style)
  // Only trigger once per tool call by checking toolCallId
  useEffect(() => {
    // Guard: already auto-opened for this toolCallId in this webview session
    // if (autoOpenedToolCallIds.has(toolCallId)) {
    //   return;
    // }
    // Only auto-open if there are diffs and we have the required data
    if (diffs.length > 0) {
      const firstDiff = diffs[0];
      const path = firstDiff.path || (locations && locations[0]?.path) || '';

      if (
        path &&
        firstDiff.oldText !== undefined &&
        firstDiff.newText !== undefined
      ) {
        // Add a small delay to ensure the component is fully rendered
        const timer = setTimeout(() => {
          handleOpenDiff(path, firstDiff.oldText, firstDiff.newText);
          // autoOpenedToolCallIds.add(toolCallId);
        }, 100);
        // Proper cleanup function
        return () => timer && clearTimeout(timer);
      }
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [toolCallId]);

  // Error case: show error
  if (errors.length > 0) {
    const path = diffs[0]?.path || locations?.[0]?.path || '';
    const fileName = path ? getFileName(path) : '';
    return (
      <ToolCallContainer
        label={fileName ? 'Edit' : 'Edit'}
        status="error"
        toolCallId={toolCallId}
        labelSuffix={
          path ? (
            <FileLink
              path={path}
              showFullPath={false}
              className="text-xs font-mono text-[var(--app-secondary-foreground)] hover:underline"
            />
          ) : undefined
        }
      >
        {errors.join('\n')}
      </ToolCallContainer>
    );
  }

  // Success case with diff: show minimal inline preview; clicking the title opens VS Code diff
  if (diffs.length > 0) {
    const firstDiff = diffs[0];
    const path = firstDiff.path || (locations && locations[0]?.path) || '';
    // const fileName = path ? getFileName(path) : '';
    const summary = getDiffSummary(firstDiff.oldText, firstDiff.newText);
    // No hooks here; define a simple click handler scoped to this block
    // const openFirstDiff = () =>
    //   handleOpenDiff(path, firstDiff.oldText, firstDiff.newText);

    const containerStatus = mapToolStatusToContainerStatus(toolCall.status);
    return (
      <div
        className={`qwen-message message-item relative py-2 select-text cursor-pointer hover:bg-[var(--app-input-background)] toolcall-container toolcall-status-${containerStatus}`}
        // onClick={openFirstDiff}
        title="Open diff in VS Code"
      >
        {/* IMPORTANT: Always include min-w-0/max-w-full on inner wrappers to prevent overflow. */}
        <div className="toolcall-edit-content flex flex-col gap-1 min-w-0 max-w-full">
          <div className="flex items-center justify-between min-w-0">
            <div className="flex items-center gap-2 min-w-0">
              {/* Align the inline Edit label styling with shared toolcall label: larger + bold */}
              <span className="text-[14px] leading-none font-bold text-[var(--app-primary-foreground)]">
                Edit
              </span>
              {path && (
                <FileLink
                  path={path}
                  showFullPath={false}
                  className="text-xs font-mono text-[var(--app-secondary-foreground)] hover:underline"
                />
              )}
            </div>
          </div>
          <div className="inline-flex text-[var(--app-secondary-foreground)] text-[0.85em] opacity-70 flex-row items-start w-full gap-1 flex items-center">
            <span className="flex-shrink-0 relative top-[-0.1em]">⎿</span>
            <span className="flex-shrink-0 w-full">{summary}</span>
          </div>

          {/* Show toolCallId only in development/debug mode */}
          {toolCallId && isDevelopmentMode() && (
            <span className="text-[10px] opacity-30">
              [{toolCallId.slice(-8)}]
            </span>
          )}
        </div>
      </div>
    );
  }

  // Success case without diff: show file in compact format
  if (locations && locations.length > 0) {
    const fileName = getFileName(locations[0].path);
    const containerStatus = mapToolStatusToContainerStatus(toolCall.status);
    return (
      <ToolCallContainer
        label={`Edited ${fileName}`}
        status={containerStatus}
        toolCallId={toolCallId}
      >
        <div className="inline-flex text-[var(--app-secondary-foreground)] text-[0.85em] opacity-70 flex-row items-start w-full gap-1 flex items-center">
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
