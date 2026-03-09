/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * WebviewView IDs for the three host positions where the chat UI can appear.
 * These IDs must match the `views` contributions declared in package.json.
 *
 * Note: We use kebab-case prefix 'qwen-code.' for consistency with command IDs.
 */
export const CHAT_VIEW_ID_SIDEBAR = 'qwen-code.chatView.sidebar';
export const CHAT_VIEW_ID_PANEL = 'qwen-code.chatView.panel';
export const CHAT_VIEW_ID_SECONDARY = 'qwen-code.chatView.secondary';
