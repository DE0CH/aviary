/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import type React from 'react';

export interface ToolCallContent {
  type: 'content' | 'diff';
  // For content type
  content?: {
    type: string;
    text?: string;
    [key: string]: unknown;
  };
  // For diff type
  path?: string;
  oldText?: string | null;
  newText?: string;
}

export interface ToolCallData {
  toolCallId: string;
  kind: string;
  title: string;
  status: 'pending' | 'in_progress' | 'completed' | 'failed';
  rawInput?: string | object;
  content?: ToolCallContent[];
  locations?: Array<{
    path: string;
    line?: number | null;
  }>;
}

export interface ToolCallProps {
  toolCall: ToolCallData;
}

const StatusTag: React.FC<{ status: string }> = ({ status }) => {
  const getStatusInfo = () => {
    switch (status) {
      case 'pending':
        return { className: 'status-pending', text: 'Pending', icon: '⏳' };
      case 'in_progress':
        return {
          className: 'status-in-progress',
          text: 'In Progress',
          icon: '🔄',
        };
      case 'completed':
        return { className: 'status-completed', text: 'Completed', icon: '✓' };
      case 'failed':
        return { className: 'status-failed', text: 'Failed', icon: '✗' };
      default:
        return { className: 'status-unknown', text: status, icon: '•' };
    }
  };

  const { className, text, icon } = getStatusInfo();
  return (
    <span className={`tool-call-status ${className}`}>
      <span className="status-icon">{icon}</span>
      {text}
    </span>
  );
};

const ContentView: React.FC<{ content: ToolCallContent }> = ({ content }) => {
  // Handle diff type
  if (content.type === 'diff') {
    const fileName =
      content.path?.split(/[/\\]/).pop() || content.path || 'Unknown file';
    const oldText = content.oldText || '';
    const newText = content.newText || '';

    return (
      <div className="tool-call-diff">
        <div className="diff-header">
          <span className="diff-icon">📝</span>
          <span className="diff-filename">{fileName}</span>
        </div>
        <div className="diff-content">
          <div className="diff-side">
            <div className="diff-side-label">Before</div>
            <pre className="diff-code">{oldText || '(empty)'}</pre>
          </div>
          <div className="diff-arrow">→</div>
          <div className="diff-side">
            <div className="diff-side-label">After</div>
            <pre className="diff-code">{newText || '(empty)'}</pre>
          </div>
        </div>
      </div>
    );
  }

  // Handle content type with text
  if (content.type === 'content' && content.content?.text) {
    return (
      <div className="tool-call-content">
        <div className="content-text">{content.content.text}</div>
      </div>
    );
  }

  return null;
};

const getKindDisplayName = (kind: string): { name: string; icon: string } => {
  const kindMap: Record<string, { name: string; icon: string }> = {
    edit: { name: 'File Edit', icon: '✏️' },
    read: { name: 'File Read', icon: '📖' },
    execute: { name: 'Shell Command', icon: '⚡' },
    fetch: { name: 'Web Fetch', icon: '🌐' },
    delete: { name: 'Delete', icon: '🗑️' },
    move: { name: 'Move/Rename', icon: '📦' },
    search: { name: 'Search', icon: '🔍' },
    think: { name: 'Thinking', icon: '💭' },
    other: { name: 'Other', icon: '🔧' },
  };

  return kindMap[kind] || { name: kind, icon: '🔧' };
};

const formatRawInput = (rawInput: string | object | undefined): string => {
  if (rawInput === undefined) {
    return '';
  }
  if (typeof rawInput === 'string') {
    return rawInput;
  }
  return JSON.stringify(rawInput, null, 2);
};

export const ToolCall: React.FC<ToolCallProps> = ({ toolCall }) => {
  const { kind, title, status, rawInput, content, locations, toolCallId } =
    toolCall;
  const kindInfo: { name: string; icon: string } = getKindDisplayName(kind);

  return (
    <div className="tool-call-card">
      <div className="tool-call-header">
        <span className="tool-call-kind-icon">{kindInfo.icon}</span>
        <span className="tool-call-title">{title || kindInfo.name}</span>
        <StatusTag status={status} />
      </div>

      {/* Show raw input if available */}
      {rawInput !== undefined && rawInput !== null ? (
        <div className="tool-call-raw-input">
          <div className="raw-input-label">Input</div>
          <pre className="raw-input-content">{formatRawInput(rawInput)}</pre>
        </div>
      ) : null}

      {/* Show locations if available */}
      {locations && locations.length > 0 && (
        <div className="tool-call-locations">
          <div className="locations-label">Files</div>
          {locations.map((location, index) => (
            <div key={index} className="location-item">
              <span className="location-icon">📄</span>
              <span className="location-path">{location.path}</span>
              {location.line !== null && location.line !== undefined && (
                <span className="location-line">:{location.line}</span>
              )}
            </div>
          ))}
        </div>
      )}

      {/* Show content if available */}
      {content && content.length > 0 && (
        <div className="tool-call-content-list">
          {content.map((item, index) => (
            <ContentView key={index} content={item} />
          ))}
        </div>
      )}

      <div className="tool-call-footer">
        <span className="tool-call-id">
          ID: {toolCallId.substring(0, 8)}...
        </span>
      </div>
    </div>
  );
};
