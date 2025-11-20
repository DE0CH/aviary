/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 *
 * Shared layout components for tool call UI
 */

import type React from 'react';
import { FileLink } from '../../shared/FileLink.js';

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
  icon: _icon,
  children,
}) => (
  <div className="tool-call-card">
    {/* <div className="tool-call-icon">{icon}</div> */}
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
 * List of file locations with clickable links
 */
export const LocationsList: React.FC<LocationsListProps> = ({ locations }) => (
  <div className="locations-list">
    {locations.map((loc, idx) => (
      <FileLink key={idx} path={loc.path} line={loc.line} showFullPath={true} />
    ))}
  </div>
);
