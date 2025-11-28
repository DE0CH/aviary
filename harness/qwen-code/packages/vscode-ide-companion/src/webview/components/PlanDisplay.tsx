/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import type React from 'react';
import './PlanDisplay.css';
import { CheckboxDisplay } from './ui/CheckboxDisplay.js';

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
  // 计算整体状态用于左侧圆点颜色
  const allCompleted =
    entries.length > 0 && entries.every((e) => e.status === 'completed');
  const anyInProgress = entries.some((e) => e.status === 'in_progress');
  const statusDotClass = allCompleted
    ? 'before:text-[#74c991]'
    : anyInProgress
      ? 'before:text-[#e1c08d]'
      : 'before:text-[var(--app-secondary-foreground)]';

  return (
    <div
      className={[
        'plan-display',
        // 容器：类似示例中的 .A/.e
        'relative flex flex-col items-start py-2 pl-[30px] select-text text-[var(--app-primary-foreground)]',
        // 左侧状态圆点，类似示例 .e:before
        'before:content-["\\25cf"] before:absolute before:left-[10px] before:top-[12px] before:text-[10px] before:z-[1]',
        statusDotClass,
      ].join(' ')}
    >
      {/* 标题区域，类似示例中的 summary/_e/or */}
      <div className="plan-header w-full">
        <div className="relative">
          <div className="list-none line-clamp-2 max-w-full overflow-hidden _e">
            <span>
              <div>
                <span className="or font-bold mr-1">Update Todos</span>
              </div>
            </span>
          </div>
        </div>
      </div>

      {/* 列表区域，类似示例中的 .qr/.Fr/.Hr */}
      <div className="qr grid-cols-1 flex flex-col py-2">
        <ul className="Fr list-none p-0 m-0 flex flex-col gap-1">
          {entries.map((entry, index) => {
            const isDone = entry.status === 'completed';
            const isIndeterminate = entry.status === 'in_progress';
            return (
              <li
                key={index}
                className={[
                  'Hr flex items-start gap-2 p-0 rounded text-[var(--app-primary-foreground)]',
                  isDone ? 'fo opacity-70' : '',
                ].join(' ')}
              >
                {/* 展示用复选框（复用组件） */}
                <label className="flex items-start gap-2">
                  <CheckboxDisplay
                    checked={isDone}
                    indeterminate={isIndeterminate}
                  />
                </label>

                <div
                  className={[
                    'vo plan-entry-text flex-1 text-xs leading-[1.5] text-[var(--app-primary-foreground)]',
                    isDone
                      ? 'line-through text-[var(--app-secondary-foreground)] opacity-70'
                      : 'opacity-85',
                  ].join(' ')}
                >
                  {entry.content}
                </div>
              </li>
            );
          })}
        </ul>
      </div>
    </div>
  );
};
