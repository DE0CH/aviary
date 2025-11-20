/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 *
 * Diff display component for showing file changes
 */

import type React from 'react';
import { useState, useMemo } from 'react';
import { FileLink } from '../../shared/FileLink.js';
import {
  calculateDiffStats,
  formatDiffStatsDetailed,
} from '../../../utils/diffStats.js';
import './DiffDisplay.css';

/**
 * Props for DiffDisplay
 */
interface DiffDisplayProps {
  path?: string;
  oldText?: string | null;
  newText?: string;
  onOpenDiff?: () => void;
  /** 默认显示模式：'compact' | 'full' */
  defaultMode?: 'compact' | 'full';
}

/**
 * Display diff with compact stats or full before/after sections
 * Supports toggling between compact and full view modes
 */
export const DiffDisplay: React.FC<DiffDisplayProps> = ({
  path,
  oldText,
  newText,
  onOpenDiff,
  defaultMode = 'compact',
}) => {
  // 视图模式状态：紧凑或完整
  const [viewMode, setViewMode] = useState<'compact' | 'full'>(defaultMode);

  // 计算 diff 统计信息（仅在文本变化时重新计算）
  const stats = useMemo(
    () => calculateDiffStats(oldText, newText),
    [oldText, newText],
  );

  // 渲染紧凑视图
  const renderCompactView = () => (
    <div className="diff-compact-view">
      <div
        className="diff-compact-clickable"
        onClick={onOpenDiff}
        role="button"
        tabIndex={0}
        title="Click to open diff in VS Code"
        onKeyDown={(e) => {
          if (e.key === 'Enter' || e.key === ' ') {
            e.preventDefault();
            onOpenDiff?.();
          }
        }}
      >
        <div className="diff-compact-header">
          {path && (
            <div className="diff-file-info">
              <FileLink
                path={path}
                showFullPath={false}
                className="diff-file-link"
                disableClick={true}
              />
            </div>
          )}
          <div className="diff-stats">
            {stats.added > 0 && (
              <span className="stat-added">+{stats.added}</span>
            )}
            {stats.removed > 0 && (
              <span className="stat-removed">-{stats.removed}</span>
            )}
            {stats.changed > 0 && (
              <span className="stat-changed">~{stats.changed}</span>
            )}
            {stats.total === 0 && (
              <span className="stat-no-change">No changes</span>
            )}
          </div>
        </div>
      </div>
      <div className="diff-compact-actions">
        <button
          className="diff-action-button secondary"
          onClick={(e) => {
            e.stopPropagation();
            setViewMode('full');
          }}
          title="Show full before/after content"
        >
          Show Details
        </button>
      </div>
    </div>
  );

  // 渲染完整视图
  const renderFullView = () => (
    <div className="diff-full-view">
      <div className="diff-header">
        <div className="diff-file-path">
          {path && <FileLink path={path} showFullPath={true} />}
        </div>
        <div className="diff-header-actions">
          {onOpenDiff && (
            <button
              className="diff-action-button primary"
              onClick={onOpenDiff}
              title="Open in VS Code diff viewer"
            >
              <svg width="14" height="14" viewBox="0 0 16 16" fill="none">
                <path d="M13.5 7l-4-4v3h-6v2h6v3l4-4z" fill="currentColor" />
              </svg>
              Open Diff
            </button>
          )}
          <button
            className="diff-action-button secondary"
            onClick={() => setViewMode('compact')}
            title="Collapse to compact view"
          >
            Collapse
          </button>
        </div>
      </div>
      <div className="diff-stats-line">{formatDiffStatsDetailed(stats)}</div>
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

  return (
    <div className="diff-display-container">
      {viewMode === 'compact' ? renderCompactView() : renderFullView()}
    </div>
  );
};
