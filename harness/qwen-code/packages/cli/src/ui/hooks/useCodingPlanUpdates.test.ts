/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { renderHook, waitFor } from '@testing-library/react';
import { useCodingPlanUpdates } from './useCodingPlanUpdates.js';
import {
  CODING_PLAN_ENV_KEY,
  CODING_PLAN_INTL_ENV_KEY,
  CODING_PLAN_BASE_URL,
  CODING_PLAN_INTL_BASE_URL,
  CODING_PLAN_VERSION,
  CODING_PLAN_INTL_VERSION,
} from '../../constants/codingPlan.js';
import { AuthType } from '@qwen-code/qwen-code-core';

describe('useCodingPlanUpdates', () => {
  const mockSettings = {
    merged: {
      modelProviders: {},
      codingPlan: {},
    },
    setValue: vi.fn(),
    isTrusted: true,
    workspace: { settings: {} },
    user: { settings: {} },
  };

  const mockConfig = {
    reloadModelProvidersConfig: vi.fn(),
    refreshAuth: vi.fn(),
  };

  const mockAddItem = vi.fn();

  beforeEach(() => {
    vi.clearAllMocks();
    delete process.env[CODING_PLAN_ENV_KEY];
    delete process.env[CODING_PLAN_INTL_ENV_KEY];
  });

  describe('version comparison', () => {
    it('should not show update prompt when no version is stored', () => {
      mockSettings.merged.codingPlan = {};

      const { result } = renderHook(() =>
        useCodingPlanUpdates(
          mockSettings as never,
          mockConfig as never,
          mockAddItem,
        ),
      );

      expect(result.current.codingPlanUpdateRequest).toBeUndefined();
    });

    it('should not show update prompt when China versions match', () => {
      mockSettings.merged.codingPlan = { version: CODING_PLAN_VERSION };

      const { result } = renderHook(() =>
        useCodingPlanUpdates(
          mockSettings as never,
          mockConfig as never,
          mockAddItem,
        ),
      );

      expect(result.current.codingPlanUpdateRequest).toBeUndefined();
    });

    it('should not show update prompt when Global versions match', () => {
      mockSettings.merged.codingPlan = {
        versionIntl: CODING_PLAN_INTL_VERSION,
      };

      const { result } = renderHook(() =>
        useCodingPlanUpdates(
          mockSettings as never,
          mockConfig as never,
          mockAddItem,
        ),
      );

      expect(result.current.codingPlanUpdateRequest).toBeUndefined();
    });

    it('should show update prompt when China versions differ', async () => {
      mockSettings.merged.codingPlan = { version: 'old-version-hash' };

      const { result } = renderHook(() =>
        useCodingPlanUpdates(
          mockSettings as never,
          mockConfig as never,
          mockAddItem,
        ),
      );

      await waitFor(() => {
        expect(result.current.codingPlanUpdateRequest).toBeDefined();
      });

      expect(result.current.codingPlanUpdateRequest?.prompt).toContain('China');
    });

    it('should show update prompt when Global versions differ', async () => {
      mockSettings.merged.codingPlan = { versionIntl: 'old-version-hash' };

      const { result } = renderHook(() =>
        useCodingPlanUpdates(
          mockSettings as never,
          mockConfig as never,
          mockAddItem,
        ),
      );

      await waitFor(() => {
        expect(result.current.codingPlanUpdateRequest).toBeDefined();
      });

      expect(result.current.codingPlanUpdateRequest?.prompt).toContain(
        'Global',
      );
    });
  });

  describe('update execution', () => {
    it('should execute China region update when user confirms', async () => {
      mockSettings.merged.codingPlan = { version: 'old-version-hash' };
      mockSettings.merged.modelProviders = {
        [AuthType.USE_OPENAI]: [
          {
            id: 'test-model-china-1',
            baseUrl: CODING_PLAN_BASE_URL,
            envKey: CODING_PLAN_ENV_KEY,
          },
          {
            id: 'custom-model',
            baseUrl: 'https://custom.example.com',
            envKey: 'CUSTOM_API_KEY',
          },
        ],
      };
      mockConfig.refreshAuth.mockResolvedValue(undefined);

      const { result } = renderHook(() =>
        useCodingPlanUpdates(
          mockSettings as never,
          mockConfig as never,
          mockAddItem,
        ),
      );

      await waitFor(() => {
        expect(result.current.codingPlanUpdateRequest).toBeDefined();
      });

      // Confirm the update
      await result.current.codingPlanUpdateRequest!.onConfirm(true);

      // Wait for async update to complete
      await waitFor(() => {
        // Should update model providers (at least 2 calls: modelProviders + version)
        expect(mockSettings.setValue).toHaveBeenCalled();
      });

      // Should update version with correct hash
      expect(mockSettings.setValue).toHaveBeenCalledWith(
        expect.anything(),
        'codingPlan.version',
        CODING_PLAN_VERSION,
      );

      // Should reload and refresh auth
      expect(mockConfig.reloadModelProvidersConfig).toHaveBeenCalled();
      expect(mockConfig.refreshAuth).toHaveBeenCalledWith(AuthType.USE_OPENAI);

      // Should show success message with region info
      expect(mockAddItem).toHaveBeenCalledWith(
        expect.objectContaining({
          type: 'info',
          text: expect.stringContaining('Coding Plan'),
        }),
        expect.any(Number),
      );
    });

    it('should execute Global region update when user confirms', async () => {
      mockSettings.merged.codingPlan = { versionIntl: 'old-version-hash' };
      mockSettings.merged.modelProviders = {
        [AuthType.USE_OPENAI]: [
          {
            id: 'test-model-global-1',
            baseUrl: CODING_PLAN_INTL_BASE_URL,
            envKey: CODING_PLAN_INTL_ENV_KEY,
          },
          {
            id: 'custom-model',
            baseUrl: 'https://custom.example.com',
            envKey: 'CUSTOM_API_KEY',
          },
        ],
      };
      mockConfig.refreshAuth.mockResolvedValue(undefined);

      const { result } = renderHook(() =>
        useCodingPlanUpdates(
          mockSettings as never,
          mockConfig as never,
          mockAddItem,
        ),
      );

      await waitFor(() => {
        expect(result.current.codingPlanUpdateRequest).toBeDefined();
      });

      // Confirm the update
      await result.current.codingPlanUpdateRequest!.onConfirm(true);

      // Wait for async update to complete
      await waitFor(() => {
        expect(mockSettings.setValue).toHaveBeenCalled();
      });

      // Should update versionIntl with correct hash
      expect(mockSettings.setValue).toHaveBeenCalledWith(
        expect.anything(),
        'codingPlan.versionIntl',
        CODING_PLAN_INTL_VERSION,
      );

      // Should reload and refresh auth
      expect(mockConfig.reloadModelProvidersConfig).toHaveBeenCalled();
      expect(mockConfig.refreshAuth).toHaveBeenCalledWith(AuthType.USE_OPENAI);

      // Should show success message with Global region info
      expect(mockAddItem).toHaveBeenCalledWith(
        expect.objectContaining({
          type: 'info',
          text: expect.stringContaining('Global'),
        }),
        expect.any(Number),
      );
    });

    it('should not execute update when user declines', async () => {
      mockSettings.merged.codingPlan = { version: 'old-version-hash' };

      const { result } = renderHook(() =>
        useCodingPlanUpdates(
          mockSettings as never,
          mockConfig as never,
          mockAddItem,
        ),
      );

      await waitFor(() => {
        expect(result.current.codingPlanUpdateRequest).toBeDefined();
      });

      // Decline the update
      await result.current.codingPlanUpdateRequest!.onConfirm(false);

      // Should not update anything
      expect(mockSettings.setValue).not.toHaveBeenCalled();
      expect(mockConfig.reloadModelProvidersConfig).not.toHaveBeenCalled();
    });

    it('should only update configs for the specific region', async () => {
      mockSettings.merged.codingPlan = { version: 'old-version-hash' };
      const chinaConfig = {
        id: 'test-model-china-1',
        baseUrl: CODING_PLAN_BASE_URL,
        envKey: CODING_PLAN_ENV_KEY,
      };
      const globalConfig = {
        id: 'test-model-global-1',
        baseUrl: CODING_PLAN_INTL_BASE_URL,
        envKey: CODING_PLAN_INTL_ENV_KEY,
      };
      const customConfig = {
        id: 'custom-model',
        baseUrl: 'https://custom.example.com',
        envKey: 'CUSTOM_API_KEY',
      };
      mockSettings.merged.modelProviders = {
        [AuthType.USE_OPENAI]: [chinaConfig, globalConfig, customConfig],
      };
      mockConfig.refreshAuth.mockResolvedValue(undefined);

      const { result } = renderHook(() =>
        useCodingPlanUpdates(
          mockSettings as never,
          mockConfig as never,
          mockAddItem,
        ),
      );

      await waitFor(() => {
        expect(result.current.codingPlanUpdateRequest).toBeDefined();
      });

      await result.current.codingPlanUpdateRequest!.onConfirm(true);

      // Wait for async update to complete
      await waitFor(() => {
        expect(mockSettings.setValue).toHaveBeenCalled();
      });

      // Get the updated configs passed to setValue
      const setValueCalls = mockSettings.setValue.mock.calls;
      const modelProvidersCall = setValueCalls.find((call: unknown[]) =>
        (call[1] as string).includes('modelProviders'),
      );

      // Should preserve Global config and custom config, only update China configs
      expect(modelProvidersCall).toBeDefined();
      const updatedConfigs = modelProvidersCall![2] as Array<
        Record<string, unknown>
      >;

      // Should have new China configs + preserved Global config + custom config
      expect(updatedConfigs.length).toBeGreaterThanOrEqual(3);

      // Should contain the Global config (not modified)
      expect(
        updatedConfigs.some(
          (c: Record<string, unknown>) => c['id'] === 'test-model-global-1',
        ),
      ).toBe(true);

      // Should contain the custom config
      expect(
        updatedConfigs.some(
          (c: Record<string, unknown>) => c['id'] === 'custom-model',
        ),
      ).toBe(true);

      // Should reload and refresh auth
      expect(mockConfig.reloadModelProvidersConfig).toHaveBeenCalled();
      expect(mockConfig.refreshAuth).toHaveBeenCalledWith(AuthType.USE_OPENAI);
    });

    it('should preserve non-Coding Plan configs during update', async () => {
      mockSettings.merged.codingPlan = { version: 'old-version-hash' };
      const customConfig = {
        id: 'custom-model',
        baseUrl: 'https://custom.example.com',
        envKey: 'CUSTOM_API_KEY',
      };
      mockSettings.merged.modelProviders = {
        [AuthType.USE_OPENAI]: [
          {
            id: 'test-model-china-1',
            baseUrl: CODING_PLAN_BASE_URL,
            envKey: CODING_PLAN_ENV_KEY,
          },
          customConfig,
        ],
      };
      mockConfig.refreshAuth.mockResolvedValue(undefined);

      const { result } = renderHook(() =>
        useCodingPlanUpdates(
          mockSettings as never,
          mockConfig as never,
          mockAddItem,
        ),
      );

      await waitFor(() => {
        expect(result.current.codingPlanUpdateRequest).toBeDefined();
      });

      await result.current.codingPlanUpdateRequest!.onConfirm(true);

      // Wait for async update to complete
      await waitFor(() => {
        // Should preserve custom config - verify setValue was called
        expect(mockSettings.setValue).toHaveBeenCalled();
      });

      // Get the updated configs passed to setValue
      const setValueCalls = mockSettings.setValue.mock.calls;
      const modelProvidersCall = setValueCalls.find((call: unknown[]) =>
        (call[1] as string).includes('modelProviders'),
      );

      // Should preserve custom config
      expect(modelProvidersCall).toBeDefined();
      const updatedConfigs = modelProvidersCall![2] as Array<
        Record<string, unknown>
      >;
      expect(
        updatedConfigs.some(
          (c: Record<string, unknown>) => c['id'] === 'custom-model',
        ),
      ).toBe(true);
    });

    it('should handle update errors gracefully', async () => {
      mockSettings.merged.codingPlan = { version: 'old-version-hash' };
      mockSettings.merged.modelProviders = {
        [AuthType.USE_OPENAI]: [
          {
            id: 'test-model-china-1',
            baseUrl: CODING_PLAN_BASE_URL,
            envKey: CODING_PLAN_ENV_KEY,
          },
        ],
      };
      // Simulate an error during refreshAuth
      mockConfig.refreshAuth.mockRejectedValue(new Error('Network error'));

      const { result } = renderHook(() =>
        useCodingPlanUpdates(
          mockSettings as never,
          mockConfig as never,
          mockAddItem,
        ),
      );

      await waitFor(() => {
        expect(result.current.codingPlanUpdateRequest).toBeDefined();
      });

      await result.current.codingPlanUpdateRequest!.onConfirm(true);

      // Should show error message
      await waitFor(() => {
        expect(mockAddItem).toHaveBeenCalledWith(
          expect.objectContaining({
            type: 'error',
          }),
          expect.any(Number),
        );
      });
    });
  });

  describe('dismissUpdate', () => {
    it('should clear update request when dismissed', async () => {
      mockSettings.merged.codingPlan = { version: 'old-version-hash' };

      const { result } = renderHook(() =>
        useCodingPlanUpdates(
          mockSettings as never,
          mockConfig as never,
          mockAddItem,
        ),
      );

      await waitFor(() => {
        expect(result.current.codingPlanUpdateRequest).toBeDefined();
      });

      result.current.dismissCodingPlanUpdate();

      await waitFor(() => {
        expect(result.current.codingPlanUpdateRequest).toBeUndefined();
      });
    });
  });
});
