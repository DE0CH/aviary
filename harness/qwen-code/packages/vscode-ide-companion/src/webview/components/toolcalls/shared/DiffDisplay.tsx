/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 *
 * Diff display component for showing file changes
 */

import type React from 'react';
import { useMemo } from 'react';
import { FileLink } from '../../shared/FileLink.js';
import {
  calculateDiffStats,
  formatDiffStatsDetailed,
} from '../../../utils/diffStats.js';
import { OpenDiffIcon } from '../../icons/index.js';
import './DiffDisplay.css';
import {
  computeLineDiff,
  truncateOps,
  type DiffOp,
} from '../../../utils/simpleDiff.js';

/**
 * Props for DiffDisplay
 */
interface DiffDisplayProps {
  path?: string;
  oldText?: string | null;
  newText?: string;
  onOpenDiff?: () => void;
  /** 是否显示统计信息 */
  showStats?: boolean;
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
  showStats = true,
}) => {
  // 统计信息（仅在文本变化时重新计算）
  const stats = useMemo(
    () => calculateDiffStats(oldText, newText),
    [oldText, newText],
  );

  // 仅生成变更行（增加/删除），不渲染上下文
  const ops: DiffOp[] = useMemo(
    () => computeLineDiff(oldText, newText),
    [oldText, newText],
  );
  const {
    items: previewOps,
    truncated,
    omitted,
  } = useMemo(() => truncateOps<DiffOp>(ops), [ops]);

  return (
    <div className="diff-display-container">
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
          {showStats && (
            <div className="diff-stats" title={formatDiffStatsDetailed(stats)}>
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
          )}
        </div>
      </div>

      {/* 只绘制差异行的预览区域 */}
      <pre className="diff-preview code-block" aria-label="Diff preview">
        <div className="code-content">
          {previewOps.length === 0 && (
            <div className="diff-line no-change">(no changes)</div>
          )}
          {previewOps.map((op, idx) => {
            if (op.type === 'add') {
              const line = op.line;
              return (
                <div key={`add-${idx}`} className="diff-line added">
                  +{line || ' '}
                </div>
              );
            }
            if (op.type === 'remove') {
              const line = op.line;
              return (
                <div key={`rm-${idx}`} className="diff-line removed">
                  -{line || ' '}
                </div>
              );
            }
            return null;
          })}
          {truncated && (
            <div
              className="diff-omitted"
              title={`${omitted} lines omitted in preview`}
            >
              … {omitted} lines omitted
            </div>
          )}
        </div>
      </pre>

      {/* 在预览下方提供显式打开按钮（可选） */}
      {onOpenDiff && (
        <div className="diff-compact-actions">
          <button
            className="diff-action-button primary"
            onClick={onOpenDiff}
            title="Open in VS Code diff viewer"
          >
            <OpenDiffIcon width="14" height="14" />
            Open Diff
          </button>
        </div>
      )}
    </div>
  );
};
