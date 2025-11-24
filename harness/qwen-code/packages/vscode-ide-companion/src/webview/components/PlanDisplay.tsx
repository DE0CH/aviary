/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import type React from 'react';
import {
  PlanCompletedIcon,
  PlanInProgressIcon,
  PlanPendingIcon,
} from './icons/index.js';
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
        return <PlanCompletedIcon className="plan-icon completed" />;
      case 'in_progress':
        return <PlanInProgressIcon className="plan-icon in-progress" />;
      default:
        // pending
        return <PlanPendingIcon className="plan-icon pending" />;
    }
  };

  return (
    <div className="plan-display">
      <div className="plan-header">
        <div className="plan-progress-icons">
          <PlanPendingIcon className="plan-progress-icon" />
          <PlanCompletedIcon className="plan-progress-icon" />
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
