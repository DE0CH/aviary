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
  /** 是否可折叠（主要用于工具输出） */
  collapsible?: boolean;
  /** 默认是否展开 */
  defaultExpanded?: boolean;
  /** 自定义标题（用于折叠时显示） */
  title?: string;
  /** 是否是最后一个项目 */
  isLast?: boolean;
}

/**
 * Timeline 项目组件 - 统一展示消息和工具调用
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
        return 'blue'; // 用户消息 - 蓝色
      case 'assistant-message':
        return 'gray'; // LLM 输出 - 灰色
      case 'tool-call':
        return 'green'; // 工具调用 - 绿色
      case 'tool-output':
        return 'gray'; // 工具输出 - 灰色
      case 'thinking':
        return 'purple'; // 思考 - 紫色
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
      {/* 时间线连接线 - 暂时禁用 */}
      {/* {!isLast && <div className="timeline-line" />} */}

      {/* 状态圆点 */}
      <div className={`timeline-dot ${dotColor}`}>
        <span className="dot-inner" />
      </div>

      {/* 内容区域 */}
      <div className="timeline-content">
        {/* 标签（可选） */}
        {itemLabel && <div className="timeline-label">{itemLabel}</div>}

        {/* 可折叠内容 */}
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
 * Timeline 容器组件
 */
export const Timeline: React.FC<{ children: React.ReactNode }> = ({
  children,
}) => <div className="timeline-container">{children}</div>;
