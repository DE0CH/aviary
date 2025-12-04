/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

export function isDevelopmentMode(): boolean {
  // TODO: 调试用
  return false;
  // return (
  //   process.env.NODE_ENV === 'development' ||
  //   process.env.DEBUG === 'true' ||
  //   process.env.NODE_ENV !== 'production'
  // );
}
