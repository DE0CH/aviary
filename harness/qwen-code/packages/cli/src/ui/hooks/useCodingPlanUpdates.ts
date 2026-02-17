/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import { useCallback, useEffect, useState } from 'react';
import type { Config, ModelProvidersConfig } from '@qwen-code/qwen-code-core';
import { AuthType } from '@qwen-code/qwen-code-core';
import type { LoadedSettings } from '../../config/settings.js';
import { getPersistScopeForModelSelection } from '../../config/modelProvidersScope.js';
import {
  isCodingPlanConfig,
  CODING_PLAN_VERSION,
  CODING_PLAN_INTL_VERSION,
  getCodingPlanConfig,
  CodingPlanRegion,
} from '../../constants/codingPlan.js';
import { t } from '../../i18n/index.js';

export interface CodingPlanUpdateRequest {
  prompt: string;
  onConfirm: (confirmed: boolean) => void;
}

/**
 * Hook for detecting and handling Coding Plan template updates.
 * Compares the persisted version with the current template version
 * and prompts the user to update if they differ.
 */
export function useCodingPlanUpdates(
  settings: LoadedSettings,
  config: Config,
  addItem: (
    item: { type: 'info' | 'error' | 'warning'; text: string },
    timestamp: number,
  ) => void,
) {
  const [updateRequest, setUpdateRequest] = useState<
    CodingPlanUpdateRequest | undefined
  >();

  /**
   * Execute the Coding Plan configuration update.
   * Removes old Coding Plan configs and replaces them with new ones from the template.
   * Automatically detects whether the user is using China or Intl version.
   */
  const executeUpdate = useCallback(
    async (region: CodingPlanRegion = CodingPlanRegion.CHINA) => {
      try {
        const persistScope = getPersistScopeForModelSelection(settings);

        // Get current configs
        const currentConfigs =
          (
            settings.merged.modelProviders as
              | Record<string, Array<Record<string, unknown>>>
              | undefined
          )?.[AuthType.USE_OPENAI] || [];

        // Filter out Coding Plan configs for the given region (keep user custom configs)
        const nonCodingPlanConfigs = currentConfigs.filter(
          (cfg) =>
            !isCodingPlanConfig(
              cfg['baseUrl'] as string | undefined,
              cfg['envKey'] as string | undefined,
              region,
            ),
        );

        // Get the correct configuration based on region
        const codingPlanConfig = getCodingPlanConfig(region);
        const { template, envKey, version } = codingPlanConfig;

        // Generate new configs from template
        const newConfigs = template.map((templateConfig) => ({
          ...templateConfig,
          envKey,
        }));

        // Combine: new Coding Plan configs at the front, user configs preserved
        const updatedConfigs = [
          ...newConfigs,
          ...(nonCodingPlanConfigs as Array<Record<string, unknown>>),
        ] as Array<Record<string, unknown>>;

        // Persist updated model providers
        settings.setValue(
          persistScope,
          `modelProviders.${AuthType.USE_OPENAI}`,
          updatedConfigs,
        );

        // Update the version with region-specific key
        const versionKey =
          region === CodingPlanRegion.GLOBAL
            ? 'codingPlan.versionIntl'
            : 'codingPlan.version';
        settings.setValue(persistScope, versionKey, version);

        // Hot-reload model providers configuration
        const updatedModelProviders = {
          ...(settings.merged.modelProviders as
            | Record<string, unknown>
            | undefined),
          [AuthType.USE_OPENAI]: updatedConfigs,
        };
        config.reloadModelProvidersConfig(
          updatedModelProviders as unknown as ModelProvidersConfig,
        );

        // Refresh auth with the new configuration
        await config.refreshAuth(AuthType.USE_OPENAI);

        const regionLabel =
          region === CodingPlanRegion.GLOBAL
            ? 'Coding Plan (Global/Intl)'
            : 'Coding Plan';
        addItem(
          {
            type: 'info',
            text: t(
              '{{region}} configuration updated successfully. New models are now available.',
              { region: regionLabel },
            ),
          },
          Date.now(),
        );

        return true;
      } catch (error) {
        const errorMessage =
          error instanceof Error ? error.message : String(error);
        addItem(
          {
            type: 'error',
            text: t('Failed to update Coding Plan configuration: {{message}}', {
              message: errorMessage,
            }),
          },
          Date.now(),
        );
        return false;
      }
    },
    [settings, config, addItem],
  );

  /**
   * Check for version mismatch and prompt user for update if needed.
   */
  const checkForUpdates = useCallback(() => {
    const mergedSettings = settings.merged as {
      codingPlan?: { version?: string; versionIntl?: string };
    };

    const savedChinaVersion = mergedSettings.codingPlan?.version;
    const savedIntlVersion = mergedSettings.codingPlan?.versionIntl;

    // Determine which version the user is using based on saved version
    // Check China version first
    if (savedChinaVersion) {
      if (savedChinaVersion !== CODING_PLAN_VERSION) {
        // China version mismatch - prompt for update
        setUpdateRequest({
          prompt: t(
            'New model configurations are available for Bailian Coding Plan (China). Update now?',
          ),
          onConfirm: async (confirmed: boolean) => {
            setUpdateRequest(undefined);
            if (confirmed) {
              await executeUpdate(CodingPlanRegion.CHINA);
            }
          },
        });
        return;
      }
    }

    // Check Intl version
    if (savedIntlVersion) {
      if (savedIntlVersion !== CODING_PLAN_INTL_VERSION) {
        // Intl version mismatch - prompt for update
        setUpdateRequest({
          prompt: t(
            'New model configurations are available for Coding Plan (Global/Intl). Update now?',
          ),
          onConfirm: async (confirmed: boolean) => {
            setUpdateRequest(undefined);
            if (confirmed) {
              await executeUpdate(CodingPlanRegion.GLOBAL);
            }
          },
        });
        return;
      }
    }

    // If no version is stored, user hasn't used Coding Plan yet - skip check
    return;
  }, [settings, executeUpdate]);

  // Check for updates on mount
  useEffect(() => {
    checkForUpdates();
  }, [checkForUpdates]);

  const dismissCodingPlanUpdate = useCallback(() => {
    setUpdateRequest(undefined);
  }, []);

  return {
    codingPlanUpdateRequest: updateRequest,
    dismissCodingPlanUpdate,
  };
}
