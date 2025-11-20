/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 *
 * Diff 统计计算工具
 */

/**
 * Diff 统计信息
 */
export interface DiffStats {
  /** 新增行数 */
  added: number;
  /** 删除行数 */
  removed: number;
  /** 修改行数（估算值） */
  changed: number;
  /** 总变更行数 */
  total: number;
}

/**
 * 计算两个文本之间的 diff 统计信息
 *
 * 使用简单的行对比算法（避免引入重量级 diff 库）
 * 算法说明：
 * 1. 将文本按行分割
 * 2. 比较行的集合差异
 * 3. 估算修改行数（同时出现在新增和删除中的行数）
 *
 * @param oldText 旧文本内容
 * @param newText 新文本内容
 * @returns diff 统计信息
 *
 * @example
 * ```typescript
 * const stats = calculateDiffStats(
 *   "line1\nline2\nline3",
 *   "line1\nline2-modified\nline4"
 * );
 * // { added: 2, removed: 2, changed: 1, total: 3 }
 * ```
 */
export function calculateDiffStats(
  oldText: string | null | undefined,
  newText: string | undefined,
): DiffStats {
  // 处理空值情况
  const oldContent = oldText || '';
  const newContent = newText || '';

  // 按行分割
  const oldLines = oldContent.split('\n').filter((line) => line.trim() !== '');
  const newLines = newContent.split('\n').filter((line) => line.trim() !== '');

  // 如果其中一个为空，直接计算
  if (oldLines.length === 0) {
    return {
      added: newLines.length,
      removed: 0,
      changed: 0,
      total: newLines.length,
    };
  }

  if (newLines.length === 0) {
    return {
      added: 0,
      removed: oldLines.length,
      changed: 0,
      total: oldLines.length,
    };
  }

  // 使用 Set 进行快速查找
  const oldSet = new Set(oldLines);
  const newSet = new Set(newLines);

  // 计算新增：在 new 中但不在 old 中的行
  const addedLines = newLines.filter((line) => !oldSet.has(line));

  // 计算删除：在 old 中但不在 new 中的行
  const removedLines = oldLines.filter((line) => !newSet.has(line));

  // 估算修改：取较小值（因为修改的行既被删除又被添加）
  // 这是一个简化的估算，实际的 diff 算法会更精确
  const estimatedChanged = Math.min(addedLines.length, removedLines.length);

  const added = addedLines.length - estimatedChanged;
  const removed = removedLines.length - estimatedChanged;
  const changed = estimatedChanged;

  return {
    added,
    removed,
    changed,
    total: added + removed + changed,
  };
}

/**
 * 格式化 diff 统计信息为人类可读的文本
 *
 * @param stats diff 统计信息
 * @returns 格式化后的文本，例如 "+5 -3 ~2"
 *
 * @example
 * ```typescript
 * formatDiffStats({ added: 5, removed: 3, changed: 2, total: 10 });
 * // "+5 -3 ~2"
 * ```
 */
export function formatDiffStats(stats: DiffStats): string {
  const parts: string[] = [];

  if (stats.added > 0) {
    parts.push(`+${stats.added}`);
  }

  if (stats.removed > 0) {
    parts.push(`-${stats.removed}`);
  }

  if (stats.changed > 0) {
    parts.push(`~${stats.changed}`);
  }

  return parts.join(' ') || 'No changes';
}

/**
 * 格式化详细的 diff 统计信息
 *
 * @param stats diff 统计信息
 * @returns 详细的描述文本
 *
 * @example
 * ```typescript
 * formatDiffStatsDetailed({ added: 5, removed: 3, changed: 2, total: 10 });
 * // "+5 lines, -3 lines, ~2 lines"
 * ```
 */
export function formatDiffStatsDetailed(stats: DiffStats): string {
  const parts: string[] = [];

  if (stats.added > 0) {
    parts.push(`+${stats.added} ${stats.added === 1 ? 'line' : 'lines'}`);
  }

  if (stats.removed > 0) {
    parts.push(`-${stats.removed} ${stats.removed === 1 ? 'line' : 'lines'}`);
  }

  if (stats.changed > 0) {
    parts.push(`~${stats.changed} ${stats.changed === 1 ? 'line' : 'lines'}`);
  }

  return parts.join(', ') || 'No changes';
}
