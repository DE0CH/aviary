/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 *
 * Diff display component for showing file changes
 */

import type React from 'react';

/**
 * Props for DiffDisplay
 */
interface DiffDisplayProps {
  path?: string;
  oldText?: string | null;
  newText?: string;
  onOpenDiff?: () => void;
}

/**
 * Display diff with before/after sections and option to open in VSCode diff viewer
 */
export const DiffDisplay: React.FC<DiffDisplayProps> = ({
  path,
  oldText,
  newText,
  onOpenDiff,
}) => (
  <div className="diff-display-container">
    <div className="diff-header">
      <div className="diff-file-path">
        <strong>{path || 'Unknown file'}</strong>
      </div>
      {onOpenDiff && (
        <button
          className="open-diff-button"
          onClick={onOpenDiff}
          title="Open in VS Code diff viewer"
        >
          <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
            <path
              d="M8 15A7 7 0 1 1 8 1a7 7 0 0 1 0 14zm0 1A8 8 0 1 0 8 0a8 8 0 0 0 0 16z"
              fill="currentColor"
            />
            <path
              d="M5.25 8a.75.75 0 0 1 .75-.75h4a.75.75 0 0 1 0 1.5H6a.75.75 0 0 1-.75-.75z"
              fill="currentColor"
            />
          </svg>
          Open Diff
        </button>
      )}
    </div>
    {oldText !== undefined && (
      <div className="diff-section">
        <div className="diff-label">Before:</div>
        <pre className="code-block">
          <div className="code-content">{oldText || '(empty)'}</div>
        </pre>
      </div>
    )}
    {newText !== undefined && (
      <div className="diff-section">
        <div className="diff-label">After:</div>
        <pre className="code-block">
          <div className="code-content">{newText}</div>
        </pre>
      </div>
    )}
  </div>
);
