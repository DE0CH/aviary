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
  // 计算完成进度
  const completedCount = entries.filter((e) => e.status === 'completed').length;
  const totalCount = entries.length;

  const getStatusIcon = (status: string) => {
    switch (status) {
      case 'completed':
        return (
          <svg
            width="14"
            height="14"
            viewBox="0 0 14 14"
            fill="none"
            className="plan-icon completed"
          >
            <circle cx="7" cy="7" r="6" fill="currentColor" opacity="0.2" />
            <path
              d="M4 7.5L6 9.5L10 4.5"
              stroke="currentColor"
              strokeWidth="1.5"
              strokeLinecap="round"
              strokeLinejoin="round"
            />
          </svg>
        );
      case 'in_progress':
        return (
          <svg
            width="14"
            height="14"
            viewBox="0 0 14 14"
            fill="none"
            className="plan-icon in-progress"
          >
            <circle
              cx="7"
              cy="7"
              r="5"
              fill="none"
              stroke="currentColor"
              strokeWidth="2.5"
            />
          </svg>
        );
      default:
        // pending
        return (
          <svg
            width="14"
            height="14"
            viewBox="0 0 14 14"
            fill="none"
            className="plan-icon pending"
          >
            <circle
              cx="7"
              cy="7"
              r="5.5"
              fill="none"
              stroke="currentColor"
              strokeWidth="1"
            />
          </svg>
        );
    }
  };

  return (
    <div className="plan-display">
      <div className="plan-header">
        <div className="plan-progress-icons">
          <svg
            width="14"
            height="14"
            viewBox="0 0 14 14"
            fill="none"
            className="plan-progress-icon"
          >
            <circle
              cx="7"
              cy="7"
              r="5.5"
              fill="none"
              stroke="currentColor"
              strokeWidth="1"
            />
          </svg>
          <svg
            width="14"
            height="14"
            viewBox="0 0 14 14"
            fill="none"
            className="plan-progress-icon"
          >
            <circle cx="7" cy="7" r="6" fill="currentColor" opacity="0.2" />
            <path
              d="M4 7.5L6 9.5L10 4.5"
              stroke="currentColor"
              strokeWidth="1.5"
              strokeLinecap="round"
              strokeLinejoin="round"
            />
          </svg>
        </div>
        <span className="plan-title">
          {completedCount} of {totalCount} Done
        </span>
      </div>
      <div className="plan-entries">
        {entries.map((entry, index) => (
          <div key={index} className={`plan-entry ${entry.status}`}>
            <div className="plan-entry-icon">{getStatusIcon(entry.status)}</div>
            <div className="plan-entry-content">
              <span className="plan-entry-text">{entry.content}</span>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
};
