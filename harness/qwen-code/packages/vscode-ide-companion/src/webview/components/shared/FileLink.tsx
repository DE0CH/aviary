/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 *
 * FileLink 组件 - 可点击的文件路径链接
 * 支持点击打开文件并跳转到指定行号和列号
 */

import type React from 'react';
import { useVSCode } from '../../hooks/useVSCode.js';
import './FileLink.css';

/**
 * Props for FileLink
 */
interface FileLinkProps {
  /** 文件路径 */
  path: string;
  /** 可选的行号（从 1 开始） */
  line?: number | null;
  /** 可选的列号（从 1 开始） */
  column?: number | null;
  /** 是否显示完整路径，默认 false（只显示文件名） */
  showFullPath?: boolean;
  /** 可选的自定义类名 */
  className?: string;
  /** 是否禁用点击行为（当父元素已经处理点击时使用） */
  disableClick?: boolean;
}

/**
 * 从完整路径中提取文件名
 * @param path 文件路径
 * @returns 文件名
 */
function getFileName(path: string): string {
  const segments = path.split(/[/\\]/);
  return segments[segments.length - 1] || path;
}

/**
 * FileLink 组件 - 可点击的文件链接
 *
 * 功能：
 * - 点击打开文件
 * - 支持行号和列号跳转
 * - 悬停显示完整路径
 * - 可选显示模式（完整路径 vs 仅文件名）
 *
 * @example
 * ```tsx
 * <FileLink path="/src/App.tsx" line={42} />
 * <FileLink path="/src/components/Button.tsx" line={10} column={5} showFullPath={true} />
 * ```
 */
export const FileLink: React.FC<FileLinkProps> = ({
  path,
  line,
  column,
  showFullPath = false,
  className = '',
  disableClick = false,
}) => {
  const vscode = useVSCode();

  /**
   * 处理点击事件 - 发送消息到 VSCode 打开文件
   */
  const handleClick = (e: React.MouseEvent) => {
    // 总是阻止默认行为（防止 <a> 标签的 # 跳转）
    e.preventDefault();

    if (disableClick) {
      // 如果禁用点击，直接返回，不阻止冒泡
      // 这样父元素可以处理点击事件
      return;
    }

    // 如果启用点击，阻止事件冒泡
    e.stopPropagation();

    // 构建包含行号和列号的完整路径
    let fullPath = path;
    if (line !== null && line !== undefined) {
      fullPath += `:${line}`;
      if (column !== null && column !== undefined) {
        fullPath += `:${column}`;
      }
    }

    console.log('[FileLink] Opening file:', fullPath);

    vscode.postMessage({
      type: 'openFile',
      data: { path: fullPath },
    });
  };

  // 构建显示文本
  const displayPath = showFullPath ? path : getFileName(path);

  // 构建悬停提示（始终显示完整路径）
  const fullDisplayText =
    line !== null && line !== undefined
      ? column !== null && column !== undefined
        ? `${path}:${line}:${column}`
        : `${path}:${line}`
      : path;

  return (
    <a
      href="#"
      className={`file-link ${disableClick ? 'file-link-disabled' : ''} ${className}`}
      onClick={handleClick}
      title={fullDisplayText}
      role="button"
      aria-label={`Open file: ${fullDisplayText}`}
    >
      <span className="file-link-path">{displayPath}</span>
      {line !== null && line !== undefined && (
        <span className="file-link-location">
          :{line}
          {column !== null && column !== undefined && <>:{column}</>}
        </span>
      )}
    </a>
  );
};
