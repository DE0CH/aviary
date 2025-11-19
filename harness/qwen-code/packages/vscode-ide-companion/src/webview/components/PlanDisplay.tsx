/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import type React from 'react';
import './PlanDisplay.css';

export interface PlanEntry {
  content: string;
  priority: 'high' | 'medium' | 'low';
  status: 'pending' | 'in_progress' | 'completed';
}

interface PlanDisplayProps {
  entries: PlanEntry[];
}

/**
 * PlanDisplay component - displays AI's task plan/todo list
 */
export const PlanDisplay: React.FC<PlanDisplayProps> = ({ entries }) => {
  const getStatusIcon = (status: string, _index: number) => {
    switch (status) {
      case 'in_progress':
        return (
          <svg
            width="16"
            height="16"
            viewBox="0 0 16 16"
            fill="none"
            className="plan-icon in-progress"
          >
            <rect
              x="2"
              y="2"
              width="12"
              height="12"
              rx="2"
              fill="var(--app-qwen-orange)"
            />
            <path
              d="M7 4L7 12M10 8L4 8"
              stroke="var(--app-qwen-ivory)"
              strokeWidth="1.5"
              strokeLinecap="round"
            />
          </svg>
        );
      case 'completed':
        return (
          <svg
            width="16"
            height="16"
            viewBox="0 0 16 16"
            fill="none"
            className="plan-icon completed"
          >
            <rect
              x="2"
              y="2"
              width="12"
              height="12"
              rx="2"
              fill="var(--app-qwen-green, #6BCF7F)"
            />
            <path
              d="M5 8L7 10L11 6"
              stroke="var(--app-qwen-ivory)"
              strokeWidth="1.5"
              strokeLinecap="round"
              strokeLinejoin="round"
            />
          </svg>
        );
      default:
        return (
          <svg
            width="16"
            height="16"
            viewBox="0 0 16 16"
            fill="none"
            className="plan-icon pending"
          >
            <rect
              x="2.5"
              y="2.5"
              width="11"
              height="11"
              rx="2"
              stroke="var(--app-secondary-foreground)"
              strokeWidth="1"
              fill="transparent"
            />
          </svg>
        );
    }
  };

  return (
    <div className="plan-display">
      <div className="plan-header">
        <svg
          width="20"
          height="20"
          viewBox="0 0 20 20"
          fill="none"
          className="plan-header-icon"
        >
          <rect
            x="3"
            y="3"
            width="14"
            height="14"
            rx="2"
            stroke="currentColor"
            strokeWidth="1.5"
            fill="none"
          />
          <path
            d="M3 7H17M7 3V7M13 3V7"
            stroke="currentColor"
            strokeWidth="1.5"
            strokeLinecap="round"
          />
        </svg>
        <span className="plan-title">Task Plan</span>
      </div>
      <div className="plan-entries">
        {entries.map((entry, index) => (
          <div key={index} className={`plan-entry ${entry.status}`}>
            <div className="plan-entry-line"></div>
            <div className="plan-entry-icon">
              {getStatusIcon(entry.status, index)}
            </div>
            <div className="plan-entry-content">
              <span className="plan-entry-number">{index + 1}.</span>
              <span className="plan-entry-text">{entry.content}</span>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
};
