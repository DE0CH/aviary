/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * ACP文件操作处理器
 *
 * 负责处理ACP协议中的文件读写操作
 */

import { promises as fs } from 'fs';
import * as path from 'path';

/**
 * ACP文件操作处理器类
 * 提供文件读写功能，符合ACP协议规范
 */
export class AcpFileHandler {
  /**
   * 处理读取文本文件请求
   *
   * @param params - 文件读取参数
   * @param params.path - 文件路径
   * @param params.sessionId - 会话ID
   * @param params.line - 起始行号（可选）
   * @param params.limit - 读取行数限制（可选）
   * @returns 文件内容
   * @throws 当文件读取失败时抛出错误
   */
  async handleReadTextFile(params: {
    path: string;
    sessionId: string;
    line: number | null;
    limit: number | null;
  }): Promise<{ content: string }> {
    console.log(`[ACP] fs/read_text_file request received for: ${params.path}`);
    console.log(`[ACP] Parameters:`, {
      line: params.line,
      limit: params.limit,
      sessionId: params.sessionId,
    });

    try {
      const content = await fs.readFile(params.path, 'utf-8');
      console.log(
        `[ACP] Successfully read file: ${params.path} (${content.length} bytes)`,
      );

      // 处理行偏移和限制
      if (params.line !== null || params.limit !== null) {
        const lines = content.split('\n');
        const startLine = params.line || 0;
        const endLine = params.limit ? startLine + params.limit : lines.length;
        const selectedLines = lines.slice(startLine, endLine);
        const result = { content: selectedLines.join('\n') };
        console.log(`[ACP] Returning ${selectedLines.length} lines`);
        return result;
      }

      const result = { content };
      console.log(`[ACP] Returning full file content`);
      return result;
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      console.error(`[ACP] Failed to read file ${params.path}:`, errorMsg);

      throw new Error(`Failed to read file '${params.path}': ${errorMsg}`);
    }
  }

  /**
   * 处理写入文本文件请求
   *
   * @param params - 文件写入参数
   * @param params.path - 文件路径
   * @param params.content - 文件内容
   * @param params.sessionId - 会话ID
   * @returns null表示成功
   * @throws 当文件写入失败时抛出错误
   */
  async handleWriteTextFile(params: {
    path: string;
    content: string;
    sessionId: string;
  }): Promise<null> {
    console.log(
      `[ACP] fs/write_text_file request received for: ${params.path}`,
    );
    console.log(`[ACP] Content size: ${params.content.length} bytes`);

    try {
      // 确保目录存在
      const dirName = path.dirname(params.path);
      console.log(`[ACP] Ensuring directory exists: ${dirName}`);
      await fs.mkdir(dirName, { recursive: true });

      // 写入文件
      await fs.writeFile(params.path, params.content, 'utf-8');

      console.log(`[ACP] Successfully wrote file: ${params.path}`);
      return null;
    } catch (error) {
      const errorMsg = error instanceof Error ? error.message : String(error);
      console.error(`[ACP] Failed to write file ${params.path}:`, errorMsg);

      throw new Error(`Failed to write file '${params.path}': ${errorMsg}`);
    }
  }
}
