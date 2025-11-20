/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

export interface VSCodeAPI {
  postMessage: (message: unknown) => void;
  getState: () => unknown;
  setState: (state: unknown) => void;
}

declare const acquireVsCodeApi: () => VSCodeAPI;

/**
 * 模块级别的 VS Code API 实例缓存
 * acquireVsCodeApi() 只能调用一次，必须在模块级别缓存
 */
let vscodeApiInstance: VSCodeAPI | null = null;

/**
 * 获取 VS Code API 实例
 * 使用模块级别缓存确保 acquireVsCodeApi() 只被调用一次
 */
function getVSCodeAPI(): VSCodeAPI {
  if (vscodeApiInstance) {
    return vscodeApiInstance;
  }

  if (typeof acquireVsCodeApi !== 'undefined') {
    vscodeApiInstance = acquireVsCodeApi();
    return vscodeApiInstance;
  }

  // Fallback for development/testing
  vscodeApiInstance = {
    postMessage: (message: unknown) => {
      console.log('Mock postMessage:', message);
    },
    getState: () => ({}),
    setState: (state: unknown) => {
      console.log('Mock setState:', state);
    },
  };
  return vscodeApiInstance;
}

/**
 * Hook to get VS Code API
 * 多个组件可以安全地调用此 hook，API 实例会被复用
 */
export function useVSCode(): VSCodeAPI {
  return getVSCodeAPI();
}
