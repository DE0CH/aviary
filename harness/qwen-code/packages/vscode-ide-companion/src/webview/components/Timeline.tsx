/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import type React from 'react';
import { useState } from 'react';
import './Timeline.css';

export type TimelineItemType =
  | 'user-message'
  | 'assistant-message'
  | 'tool-call'
  | 'tool-output'
  | 'thinking';

export interface TimelineItemProps {
  type: TimelineItemType;
  children: React.ReactNode;
  /** Whether collapsible (mainly for tool output) */
  collapsible?: boolean;
  /** Default expanded */
  defaultExpanded?: boolean;
  /** Custom title (used for display when collapsed) */
  title?: string;
  /** Whether it is the last item */
  isLast?: boolean;
}

/**
 * Timeline item component - Unified display of messages and tool calls
 */
export const TimelineItem: React.FC<TimelineItemProps> = ({
  type,
  children,
  collapsible = false,
  defaultExpanded = true,
  title,
  isLast = false,
}) => {
  const [expanded, setExpanded] = useState(defaultExpanded);

  const getDotColor = (): string => {
    switch (type) {
      case 'user-message':
        return 'blue'; // User message - Blue
      case 'assistant-message':
        return 'gray'; // LLM output - Gray
      case 'tool-call':
        return 'green'; // Tool call - Green
      case 'tool-output':
        return 'gray'; // Tool output - Gray
      case 'thinking':
        return 'purple'; // Thinking - Purple
      default:
        return 'gray';
    }
  };

  const getItemLabel = (): string | null => {
    switch (type) {
      case 'user-message':
        return 'You';
      case 'assistant-message':
        return 'Qwen';
      case 'tool-call':
        return 'Tool Call';
      case 'tool-output':
        return 'Output';
      case 'thinking':
        return 'Thinking';
      default:
        return null;
    }
  };

  const dotColor = getDotColor();
  const itemLabel = getItemLabel();

  return (
    <div className={`timeline-item ${type} ${isLast ? 'last' : ''}`}>
      {/* Timeline connecting line - Temporarily disabled */}
      {/* {!isLast && <div className="timeline-line" />} */}

      {/* Status dot */}
      <div className={`timeline-dot ${dotColor}`}>
        <span className="dot-inner" />
      </div>

      {/* Content area */}
      <div className="timeline-content">
        {/* Label (optional) */}
        {itemLabel && <div className="timeline-label">{itemLabel}</div>}

        {/* Collapsible content */}
        {collapsible ? (
          <>
            <button
              className="timeline-toggle"
              onClick={() => setExpanded(!expanded)}
              aria-expanded={expanded}
            >
              <span className="timeline-toggle-icon">
                {expanded ? '▼' : '▶'}
              </span>
              <span className="timeline-toggle-text">
                {title || (expanded ? 'Hide details' : 'Show details')}
              </span>
            </button>
            {expanded && <div className="timeline-body">{children}</div>}
          </>
        ) : (
          <div className="timeline-body">{children}</div>
        )}
      </div>
    </div>
  );
};

/**
 * Timeline container component
 */
export const Timeline: React.FC<{ children: React.ReactNode }> = ({
  children,
}) => <div className="timeline-container">{children}</div>;
