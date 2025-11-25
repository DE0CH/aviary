/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 *
 * FileLink component - Clickable file path links
 * Supports clicking to open files and jump to specified line and column numbers
 */

import type React from 'react';
import { useVSCode } from '../../hooks/useVSCode.js';
import './FileLink.css';

/**
 * Props for FileLink
 */
interface FileLinkProps {
  /** File path */
  path: string;
  /** Optional line number (starting from 1) */
  line?: number | null;
  /** Optional column number (starting from 1) */
  column?: number | null;
  /** Whether to show full path, default false (show filename only) */
  showFullPath?: boolean;
  /** Optional custom class name */
  className?: string;
  /** Whether to disable click behavior (use when parent element handles clicks) */
  disableClick?: boolean;
}

/**
 * Extract filename from full path
 * @param path File path
 * @returns Filename
 */
function getFileName(path: string): string {
  const segments = path.split(/[/\\]/);
  return segments[segments.length - 1] || path;
}

/**
 * FileLink component - Clickable file link
 *
 * Features:
 * - Click to open file
 * - Support line and column number navigation
 * - Hover to show full path
 * - Optional display mode (full path vs filename only)
 *
 * @example
 * ```tsx
 * <FileLink path="/src/App.tsx" line={42} />
 * <FileLink path="/src/components/Button.tsx" line={10} column={5} showFullPath={true} />
 * ```
 */
export const FileLink: React.FC<FileLinkProps> = ({
  path,
  line,
  column,
  showFullPath = false,
  className = '',
  disableClick = false,
}) => {
  const vscode = useVSCode();

  /**
   * Handle click event - Send message to VSCode to open file
   */
  const handleClick = (e: React.MouseEvent) => {
    // Always prevent default behavior (prevent <a> tag # navigation)
    e.preventDefault();

    if (disableClick) {
      // If click is disabled, return directly without stopping propagation
      // This allows parent elements to handle click events
      return;
    }

    // If click is enabled, stop event propagation
    e.stopPropagation();

    // Build full path including line and column numbers
    let fullPath = path;
    if (line !== null && line !== undefined) {
      fullPath += `:${line}`;
      if (column !== null && column !== undefined) {
        fullPath += `:${column}`;
      }
    }

    console.log('[FileLink] Opening file:', fullPath);

    vscode.postMessage({
      type: 'openFile',
      data: { path: fullPath },
    });
  };

  // Build display text
  const displayPath = showFullPath ? path : getFileName(path);

  // Build hover tooltip (always show full path)
  const fullDisplayText =
    line !== null && line !== undefined
      ? column !== null && column !== undefined
        ? `${path}:${line}:${column}`
        : `${path}:${line}`
      : path;

  return (
    <a
      href="#"
      className={`file-link ${disableClick ? 'file-link-disabled' : ''} ${className}`}
      onClick={handleClick}
      title={fullDisplayText}
      role="button"
      aria-label={`Open file: ${fullDisplayText}`}
    >
      <span className="file-link-path">{displayPath}</span>
      {line !== null && line !== undefined && (
        <span className="file-link-location">
          :{line}
          {column !== null && column !== undefined && <>:{column}</>}
        </span>
      )}
    </a>
  );
};
