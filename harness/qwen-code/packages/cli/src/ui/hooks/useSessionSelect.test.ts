/**
 * @license
 * Copyright 2025 Qwen Code
 * SPDX-License-Identifier: Apache-2.0
 */

import { act, renderHook } from '@testing-library/react';
import { describe, it, expect, vi } from 'vitest';
import { useSessionSelect } from './useSessionSelect.js';

vi.mock('../utils/resumeHistoryUtils.js', () => ({
  buildResumedHistoryItems: vi.fn(() => [{ id: 1, type: 'user', text: 'hi' }]),
}));

vi.mock('@qwen-code/qwen-code-core', () => {
  class SessionService {
    constructor(_cwd: string) {}
    async loadSession(_sessionId: string) {
      return { conversation: [{ role: 'user', parts: [{ text: 'hello' }] }] };
    }
  }

  return {
    SessionService,
    buildApiHistoryFromConversation: vi.fn(() => [{ role: 'user', parts: [] }]),
    replayUiTelemetryFromConversation: vi.fn(),
    uiTelemetryService: { reset: vi.fn() },
  };
});

describe('useSessionSelect', () => {
  it('no-ops when config is null', async () => {
    const closeResumeDialog = vi.fn();
    const historyManager = { clearItems: vi.fn(), loadHistory: vi.fn() };
    const startNewSession = vi.fn();

    const { result } = renderHook(() =>
      useSessionSelect({
        config: null,
        closeResumeDialog,
        historyManager,
        startNewSession,
      }),
    );

    await act(async () => {
      await result.current('session-1');
    });

    expect(closeResumeDialog).not.toHaveBeenCalled();
    expect(startNewSession).not.toHaveBeenCalled();
    expect(historyManager.clearItems).not.toHaveBeenCalled();
    expect(historyManager.loadHistory).not.toHaveBeenCalled();
  });

  it('closes the dialog immediately and restores session state', async () => {
    const closeResumeDialog = vi.fn();
    const historyManager = { clearItems: vi.fn(), loadHistory: vi.fn() };
    const startNewSession = vi.fn();
    const geminiClient = {
      initialize: vi.fn(),
    };

    const config = {
      getTargetDir: () => '/tmp',
      getGeminiClient: () => geminiClient,
      startNewSession: vi.fn(),
    } as unknown as import('@qwen-code/qwen-code-core').Config;

    const { result } = renderHook(() =>
      useSessionSelect({
        config,
        closeResumeDialog,
        historyManager,
        startNewSession,
      }),
    );

    const resumePromise = act(async () => {
      await result.current('session-2');
    });

    expect(closeResumeDialog).toHaveBeenCalledTimes(1);
    await resumePromise;

    expect(config.startNewSession).toHaveBeenCalledWith(
      'session-2',
      expect.objectContaining({
        conversation: expect.anything(),
      }),
    );
    expect(startNewSession).toHaveBeenCalledWith('session-2');
    expect(geminiClient.initialize).toHaveBeenCalledTimes(1);
    expect(historyManager.clearItems).toHaveBeenCalledTimes(1);
    expect(historyManager.loadHistory).toHaveBeenCalledTimes(1);
  });
});
