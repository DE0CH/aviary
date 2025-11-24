/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 *
 * In-progress tool call component - displays active tool calls with Claude Code style
 */

import type React from 'react';
import type { ToolCallData } from './toolcalls/shared/types.js';
import { FileLink } from './shared/FileLink.js';

interface InProgressToolCallProps {
  toolCall: ToolCallData;
}

/**
 * Format the kind name to a readable label
 */
const formatKind = (kind: string): string => {
  const kindMap: Record<string, string> = {
    read: 'Read',
    write: 'Write',
    edit: 'Edit',
    execute: 'Execute',
    bash: 'Execute',
    command: 'Execute',
    search: 'Search',
    grep: 'Search',
    glob: 'Search',
    find: 'Search',
    think: 'Think',
    thinking: 'Think',
    fetch: 'Fetch',
    delete: 'Delete',
    move: 'Move',
  };

  return kindMap[kind.toLowerCase()] || 'Tool Call';
};

/**
 * Get status display text
 */
const getStatusText = (status: string): string => {
  const statusMap: Record<string, string> = {
    pending: 'Pending',
    in_progress: 'In Progress',
    completed: 'Completed',
    failed: 'Failed',
  };

  return statusMap[status] || status;
};

/**
 * Component to display in-progress tool calls with Claude Code styling
 * Shows kind, status, and file locations
 */
export const InProgressToolCall: React.FC<InProgressToolCallProps> = ({
  toolCall,
}) => {
  const { kind, status, title, locations } = toolCall;

  // Format the kind label
  const kindLabel = formatKind(kind);

  // Get status text
  const statusText = getStatusText(status || 'in_progress');

  // Safely prepare a display value for title. Titles may sometimes arrive as
  // non-string objects; ensure we render a string in that case.
  const titleText = typeof title === 'string' ? title : undefined;
  const titleDisplay: React.ReactNode =
    typeof title === 'string' ? title : title ? JSON.stringify(title) : null;

  return (
    <div className="in-progress-tool-call">
      <div className="in-progress-tool-call-header">
        <span className="in-progress-tool-call-kind">{kindLabel}</span>
        <span
          className={`in-progress-tool-call-status ${status || 'in_progress'}`}
        >
          {statusText}
        </span>
      </div>

      {titleDisplay && (titleText ? titleText !== kindLabel : true) && (
        <div className="in-progress-tool-call-title">{titleDisplay}</div>
      )}

      {locations && locations.length > 0 && (
        <div className="in-progress-tool-call-locations">
          {locations.map((loc, idx) => (
            <FileLink
              key={idx}
              path={loc.path}
              line={loc.line}
              showFullPath={false}
            />
          ))}
        </div>
      )}
    </div>
  );
};
