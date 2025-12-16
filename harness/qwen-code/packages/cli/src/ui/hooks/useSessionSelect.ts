/**
 * @license
 * Copyright 2025 Qwen Code
 * SPDX-License-Identifier: Apache-2.0
 */

import { useCallback } from 'react';
import { SessionService, type Config } from '@qwen-code/qwen-code-core';
import { buildResumedHistoryItems } from '../utils/resumeHistoryUtils.js';
import type { UseHistoryManagerReturn } from './useHistoryManager.js';

export interface UseSessionSelectOptions {
  config: Config | null;
  historyManager: Pick<UseHistoryManagerReturn, 'clearItems' | 'loadHistory'>;
  closeResumeDialog: () => void;
  startNewSession: (sessionId: string) => void;
  remount?: () => void;
}

/**
 * Returns a stable callback to resume a saved session and restore UI + client state.
 */
export function useSessionSelect({
  config,
  closeResumeDialog,
  historyManager,
  startNewSession,
  remount,
}: UseSessionSelectOptions): (sessionId: string) => void {
  return useCallback(
    async (sessionId: string) => {
      if (!config) {
        return;
      }

      // Close dialog immediately to prevent input capture during async operations.
      closeResumeDialog();

      const cwd = config.getTargetDir();
      const sessionService = new SessionService(cwd);
      const sessionData = await sessionService.loadSession(sessionId);

      if (!sessionData) {
        return;
      }

      // Start new session in UI context.
      startNewSession(sessionId);

      // Reset UI history.
      const uiHistoryItems = buildResumedHistoryItems(sessionData, config);
      historyManager.clearItems();
      historyManager.loadHistory(uiHistoryItems);

      // Update session history core.
      config.startNewSession(sessionId, sessionData);
      await config.getGeminiClient()?.initialize?.();

      // Refresh terminal UI.
      remount?.();
    },
    [closeResumeDialog, config, historyManager, startNewSession, remount],
  );
}
