/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 *
 * Shared layout components for tool call UI
 */

import type React from 'react';

/**
 * Props for ToolCallCard wrapper
 */
interface ToolCallCardProps {
  icon: string;
  children: React.ReactNode;
}

/**
 * Main card wrapper with icon
 */
export const ToolCallCard: React.FC<ToolCallCardProps> = ({
  icon,
  children,
}) => (
  <div className="tool-call-card">
    <div className="tool-call-icon">{icon}</div>
    <div className="tool-call-grid">{children}</div>
  </div>
);

/**
 * Props for ToolCallRow
 */
interface ToolCallRowProps {
  label: string;
  children: React.ReactNode;
}

/**
 * A single row in the tool call grid
 */
export const ToolCallRow: React.FC<ToolCallRowProps> = ({
  label,
  children,
}) => (
  <div className="tool-call-row">
    <div className="tool-call-label">{label}</div>
    <div className="tool-call-value">{children}</div>
  </div>
);

/**
 * Props for StatusIndicator
 */
interface StatusIndicatorProps {
  status: 'pending' | 'in_progress' | 'completed' | 'failed';
  text: string;
}

/**
 * Status indicator with colored dot
 */
export const StatusIndicator: React.FC<StatusIndicatorProps> = ({
  status,
  text,
}) => (
  <div className={`tool-call-status-indicator ${status}`} title={status}>
    {text}
  </div>
);

/**
 * Props for CodeBlock
 */
interface CodeBlockProps {
  children: string;
}

/**
 * Code block for displaying formatted code or output
 */
export const CodeBlock: React.FC<CodeBlockProps> = ({ children }) => (
  <pre className="code-block">{children}</pre>
);

/**
 * Props for LocationsList
 */
interface LocationsListProps {
  locations: Array<{
    path: string;
    line?: number | null;
  }>;
}

/**
 * List of file locations
 */
export const LocationsList: React.FC<LocationsListProps> = ({ locations }) => (
  <>
    {locations.map((loc, idx) => (
      <div key={idx}>
        {loc.path}
        {loc.line !== null && loc.line !== undefined && `:${loc.line}`}
      </div>
    ))}
  </>
);

/**
 * Props for DiffDisplay
 */
interface DiffDisplayProps {
  path?: string;
  oldText?: string | null;
  newText?: string;
}

/**
 * Display diff with before/after sections
 */
export const DiffDisplay: React.FC<DiffDisplayProps> = ({
  path,
  oldText,
  newText,
}) => (
  <div>
    <div>
      <strong>{path || 'Unknown file'}</strong>
    </div>
    {oldText !== undefined && (
      <div>
        <div
          style={{
            opacity: 0.5,
            fontSize: '0.85em',
            marginTop: '4px',
          }}
        >
          Before:
        </div>
        <pre className="code-block">{oldText || '(empty)'}</pre>
      </div>
    )}
    {newText !== undefined && (
      <div>
        <div
          style={{
            opacity: 0.5,
            fontSize: '0.85em',
            marginTop: '4px',
          }}
        >
          After:
        </div>
        <pre className="code-block">{newText}</pre>
      </div>
    )}
  </div>
);
