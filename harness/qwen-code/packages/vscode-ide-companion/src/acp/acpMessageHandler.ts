/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * ACP消息处理器
 *
 * 负责处理ACP协议中的消息接收、解析和分发
 */

import type {
  AcpMessage,
  AcpRequest,
  AcpNotification,
  AcpResponse,
  AcpSessionUpdate,
  AcpPermissionRequest,
} from '../shared/acpTypes.js';
import { CLIENT_METHODS } from './schema.js';
import type {
  PendingRequest,
  AcpConnectionCallbacks,
} from './connectionTypes.js';
import { AcpFileHandler } from './acpFileHandler.js';
import type { ChildProcess } from 'child_process';

/**
 * ACP消息处理器类
 * 负责消息的接收、解析和处理
 */
export class AcpMessageHandler {
  private fileHandler: AcpFileHandler;

  constructor() {
    this.fileHandler = new AcpFileHandler();
  }

  /**
   * 发送响应消息到子进程
   *
   * @param child - 子进程实例
   * @param response - 响应消息
   */
  sendResponseMessage(child: ChildProcess | null, response: AcpResponse): void {
    if (child?.stdin) {
      const jsonString = JSON.stringify(response);
      const lineEnding = process.platform === 'win32' ? '\r\n' : '\n';
      child.stdin.write(jsonString + lineEnding);
    }
  }

  /**
   * 处理接收到的消息
   *
   * @param message - ACP消息
   * @param pendingRequests - 待处理请求映射表
   * @param callbacks - 回调函数集合
   */
  handleMessage(
    message: AcpMessage,
    pendingRequests: Map<number, PendingRequest<unknown>>,
    callbacks: AcpConnectionCallbacks,
  ): void {
    try {
      if ('method' in message) {
        // 请求或通知
        this.handleIncomingRequest(message, callbacks).catch(() => {});
      } else if (
        'id' in message &&
        typeof message.id === 'number' &&
        pendingRequests.has(message.id)
      ) {
        // 响应
        this.handleResponse(message, pendingRequests, callbacks);
      }
    } catch (error) {
      console.error('[ACP] Error handling message:', error);
    }
  }

  /**
   * 处理响应消息
   *
   * @param message - 响应消息
   * @param pendingRequests - 待处理请求映射表
   * @param callbacks - 回调函数集合
   */
  private handleResponse(
    message: AcpMessage,
    pendingRequests: Map<number, PendingRequest<unknown>>,
    callbacks: AcpConnectionCallbacks,
  ): void {
    if (!('id' in message) || typeof message.id !== 'number') {
      return;
    }

    const pendingRequest = pendingRequests.get(message.id);
    if (!pendingRequest) {
      return;
    }

    const { resolve, reject, method } = pendingRequest;
    pendingRequests.delete(message.id);

    if ('result' in message) {
      console.log(
        `[ACP] Response for ${method}:`,
        JSON.stringify(message.result).substring(0, 200),
      );
      if (
        message.result &&
        typeof message.result === 'object' &&
        'stopReason' in message.result &&
        message.result.stopReason === 'end_turn'
      ) {
        callbacks.onEndTurn();
      }
      resolve(message.result);
    } else if ('error' in message) {
      const errorCode = message.error?.code || 'unknown';
      const errorMsg = message.error?.message || 'Unknown ACP error';
      const errorData = message.error?.data
        ? JSON.stringify(message.error.data)
        : '';
      console.error(`[ACP] Error response for ${method}:`, {
        code: errorCode,
        message: errorMsg,
        data: errorData,
      });
      reject(
        new Error(
          `${errorMsg} (code: ${errorCode})${errorData ? '\nData: ' + errorData : ''}`,
        ),
      );
    }
  }

  /**
   * 处理进入的请求
   *
   * @param message - 请求或通知消息
   * @param callbacks - 回调函数集合
   * @returns 请求处理结果
   */
  async handleIncomingRequest(
    message: AcpRequest | AcpNotification,
    callbacks: AcpConnectionCallbacks,
  ): Promise<unknown> {
    const { method, params } = message;

    let result = null;

    switch (method) {
      case CLIENT_METHODS.session_update:
        callbacks.onSessionUpdate(params as AcpSessionUpdate);
        break;
      case CLIENT_METHODS.session_request_permission:
        result = await this.handlePermissionRequest(
          params as AcpPermissionRequest,
          callbacks,
        );
        break;
      case CLIENT_METHODS.fs_read_text_file:
        result = await this.fileHandler.handleReadTextFile(
          params as {
            path: string;
            sessionId: string;
            line: number | null;
            limit: number | null;
          },
        );
        break;
      case CLIENT_METHODS.fs_write_text_file:
        result = await this.fileHandler.handleWriteTextFile(
          params as { path: string; content: string; sessionId: string },
        );
        break;
      default:
        console.warn(`[ACP] Unhandled method: ${method}`);
        break;
    }

    return result;
  }

  /**
   * 处理权限请求
   *
   * @param params - 权限请求参数
   * @param callbacks - 回调函数集合
   * @returns 权限请求结果
   */
  private async handlePermissionRequest(
    params: AcpPermissionRequest,
    callbacks: AcpConnectionCallbacks,
  ): Promise<{
    outcome: { outcome: string; optionId: string };
  }> {
    try {
      const response = await callbacks.onPermissionRequest(params);
      const optionId = response.optionId;

      // 处理取消、拒绝或允许
      let outcome: string;
      if (optionId.includes('reject') || optionId === 'cancel') {
        outcome = 'rejected';
      } else {
        outcome = 'selected';
      }

      return {
        outcome: {
          outcome,
          optionId: optionId === 'cancel' ? 'reject_once' : optionId,
        },
      };
    } catch (_error) {
      return {
        outcome: {
          outcome: 'rejected',
          optionId: 'reject_once',
        },
      };
    }
  }
}
