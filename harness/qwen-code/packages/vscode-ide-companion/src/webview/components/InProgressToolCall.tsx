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

      {title && title !== kindLabel && (
        <div className="in-progress-tool-call-title">{title}</div>
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
