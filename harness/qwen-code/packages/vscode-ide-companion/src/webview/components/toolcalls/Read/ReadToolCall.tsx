/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 *
 * Read tool call component - specialized for file reading operations
 */

import type React from 'react';
import { useCallback, useEffect, useMemo } from 'react';
import type { BaseToolCallProps } from '../shared/types.js';
import {
  groupContent,
  mapToolStatusToContainerStatus,
} from '../shared/utils.js';
import { FileLink } from '../../ui/FileLink.js';
import { useVSCode } from '../../../hooks/useVSCode.js';
import { handleOpenDiff } from '../../../utils/diffUtils.js';

/**
 * Specialized component for Read tool calls
 * Optimized for displaying file reading operations
 * Shows: Read filename (no content preview)
 */
export const ReadToolCall: React.FC<BaseToolCallProps> = ({
  toolCall,
  isFirst,
  isLast,
}) => {
  const { content, locations, toolCallId } = toolCall;
  const vscode = useVSCode();

  // Group content by type; memoize to avoid new array identities on every render
  const { errors, diffs } = useMemo(() => groupContent(content), [content]);

  // Post a message to the extension host to open a VS Code diff tab
  const handleOpenDiffInternal = useCallback(
    (
      path: string | undefined,
      oldText: string | null | undefined,
      newText: string | undefined,
    ) => {
      handleOpenDiff(vscode, path, oldText, newText);
    },
    [vscode],
  );

  // Auto-open diff (Claude-style) when a read call returns diff content.
  // Only trigger once per toolCallId so we don't spam as in-progress updates stream in.
  useEffect(() => {
    if (diffs.length > 0) {
      const firstDiff = diffs[0];
      const path = firstDiff.path || (locations && locations[0]?.path) || '';

      if (
        path &&
        firstDiff.oldText !== undefined &&
        firstDiff.newText !== undefined
      ) {
        const timer = setTimeout(() => {
          handleOpenDiffInternal(path, firstDiff.oldText, firstDiff.newText);
        }, 100);
        return () => timer && clearTimeout(timer);
      }
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [toolCallId]);

  // Compute container status based on toolCall.status (pending/in_progress -> loading)
  const containerStatus:
    | 'success'
    | 'error'
    | 'warning'
    | 'loading'
    | 'default' = mapToolStatusToContainerStatus(toolCall.status);

  // Compute pseudo-element classes for status dot (use ::before per requirement)
  const beforeStatusClass =
    containerStatus === 'success'
      ? 'before:text-qwen-success'
      : containerStatus === 'error'
        ? 'before:text-qwen-error'
        : containerStatus === 'warning'
          ? 'before:text-qwen-warning'
          : 'before:text-qwen-loading before:opacity-70 before:animate-pulse-slow';

  const ReadContainer: React.FC<{
    status: typeof containerStatus;
    path?: string;
    children?: React.ReactNode;
    isError?: boolean;
  }> = ({ status, path, children, isError }) => {
    // Adjust the connector line to crop for first/last items
    const lineCropTop = isFirst ? 'top-[24px]' : 'top-0';
    const lineCropBottom = isLast ? 'bottom-auto h-[calc(100%-24px)]' : 'bottom-0';
    return (
      <div
        className={
          `qwen-message message-item relative pl-[30px] py-2 select-text ` +
          `before:absolute before:left-[8px] before:top-2 before:content-["\\25cf"] before:text-[10px] before:z-[1] ` +
          beforeStatusClass
        }
      >
        {/* timeline vertical line */}
        <div
          className={`absolute left-[12px] ${lineCropTop} ${lineCropBottom} w-px bg-[var(--app-primary-border-color)]`}
          aria-hidden
        />
        <div className="flex-1 min-w-0">
          <div className="flex items-baseline gap-2 min-w-0">
            <span className="text-[14px] leading-none font-bold text-[var(--app-primary-foreground)]">
              Read
            </span>
            {path ? (
              <FileLink
                path={path}
                showFullPath={false}
                className="text-xs font-mono text-[var(--app-secondary-foreground)] hover:underline"
              />
            ) : null}
          </div>
          {children ? (
            <div
              className={`mt-1 text-[var(--app-secondary-foreground)] ${
                isError ? 'text-qwen-error' : ''
              }`}
            >
              {children}
            </div>
          ) : null}
        </div>
      </div>
    );
  };

  // Error case: show error
  if (errors.length > 0) {
    const path = locations?.[0]?.path || '';
    return (
      <ReadContainer status="error" path={path} isError>
        {errors.join('\n')}
      </ReadContainer>
    );
  }

  // Success case with diff: keep UI compact; VS Code diff is auto-opened above
  if (diffs.length > 0) {
    const path = diffs[0]?.path || locations?.[0]?.path || '';
    return <ReadContainer status={containerStatus} path={path} />;
  }

  // Success case: show which file was read with filename in label
  if (locations && locations.length > 0) {
    const path = locations[0].path;
    return <ReadContainer status={containerStatus} path={path} />;
  }

  // No file info, don't show
  return null;
};
