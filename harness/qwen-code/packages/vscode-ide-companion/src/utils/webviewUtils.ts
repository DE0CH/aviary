/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * 从完整路径中提取文件名
 * @param fsPath 文件的完整路径
 * @returns 文件名（不含路径）
 */
export function getFileName(fsPath: string): string {
  // 使用 path.basename 的逻辑：找到最后一个路径分隔符后的部分
  const lastSlash = Math.max(fsPath.lastIndexOf('/'), fsPath.lastIndexOf('\\'));
  return lastSlash >= 0 ? fsPath.substring(lastSlash + 1) : fsPath;
}

/**
 * HTML 转义函数，防止 XSS 攻击
 * 将特殊字符转换为 HTML 实体
 * @param text 需要转义的文本
 * @returns 转义后的文本
 */
export function escapeHtml(text: string): string {
  return text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#039;');
}
