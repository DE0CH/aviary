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
  const getPriorityIcon = (priority: string) => {
    switch (priority) {
      case 'high':
        return '🔴';
      case 'medium':
        return '🟡';
      case 'low':
        return '🟢';
      default:
        return '⚪';
    }
  };

  const getStatusIcon = (status: string) => {
    switch (status) {
      case 'pending':
        return '⏱️';
      case 'in_progress':
        return '⚙️';
      case 'completed':
        return '✅';
      default:
        return '❓';
    }
  };

  return (
    <div className="plan-display">
      <div className="plan-header">
        <span className="plan-icon">📋</span>
        <span className="plan-title">Task Plan</span>
      </div>
      <div className="plan-entries">
        {entries.map((entry, index) => (
          <div
            key={index}
            className={`plan-entry ${entry.status}`}
            data-priority={entry.priority}
          >
            <div className="plan-entry-header">
              <span className="plan-entry-status">
                {getStatusIcon(entry.status)}
              </span>
              <span className="plan-entry-priority">
                {getPriorityIcon(entry.priority)}
              </span>
              <span className="plan-entry-index">{index + 1}.</span>
            </div>
            <div className="plan-entry-content">{entry.content}</div>
          </div>
        ))}
      </div>
    </div>
  );
};
