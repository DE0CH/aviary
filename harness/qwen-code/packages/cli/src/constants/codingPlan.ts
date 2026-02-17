/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import { createHash } from 'node:crypto';
import type { ProviderModelConfig as ModelConfig } from '@qwen-code/qwen-code-core';

/**
 * Coding plan regions
 */
export enum CodingPlanRegion {
  CHINA = 'china',
  GLOBAL = 'global',
}

/**
 * Coding plan template - array of model configurations
 * When user provides an api-key, these configs will be cloned with envKey pointing to the stored api-key
 */
export type CodingPlanTemplate = ModelConfig[];

/**
 * Environment variable key for storing the coding plan API key (China/Bailian)
 */
export const CODING_PLAN_ENV_KEY = 'BAILIAN_CODING_PLAN_API_KEY';

/**
 * Environment variable key for storing the coding plan API key (Global/Intl)
 */
export const CODING_PLAN_INTL_ENV_KEY = 'BAILIAN_CODING_PLAN_INTL_API_KEY';

/**
 * Base URL for China/Bailian Coding Plan
 */
export const CODING_PLAN_BASE_URL = 'https://coding.dashscope.aliyuncs.com/v1';

/**
 * Base URL for Global/Intl Coding Plan
 */
export const CODING_PLAN_INTL_BASE_URL =
  'https://coding-intl.dashscope.aliyuncs.com/v1';

/**
 * CODING_PLAN_MODELS defines the model configurations for coding-plan mode (China/Bailian).
 */
export const CODING_PLAN_MODELS: CodingPlanTemplate = [
  {
    id: 'qwen3-coder-plus',
    name: 'qwen3-coder-plus',
    baseUrl: CODING_PLAN_BASE_URL,
    description: 'qwen3-coder-plus model from Bailian Coding Plan',
    envKey: CODING_PLAN_ENV_KEY,
  },
  {
    id: 'qwen3-max-2026-01-23',
    name: 'qwen3-max-2026-01-23',
    description:
      'qwen3-max model with thinking enabled from Bailian Coding Plan',
    baseUrl: CODING_PLAN_BASE_URL,
    envKey: CODING_PLAN_ENV_KEY,
    generationConfig: {
      extra_body: {
        enable_thinking: true,
      },
    },
  },
];

/**
 * CODING_PLAN_INTL_MODELS defines the model configurations for coding-plan mode (Global/Intl).
 */
export const CODING_PLAN_INTL_MODELS: CodingPlanTemplate = [
  {
    id: 'qwen3-coder-plus',
    name: 'qwen3-coder-plus',
    baseUrl: CODING_PLAN_INTL_BASE_URL,
    description: 'qwen3-coder-plus model from Coding Plan (Global/Intl)',
    envKey: CODING_PLAN_INTL_ENV_KEY,
  },
  {
    id: 'qwen3-max-2026-01-23',
    name: 'qwen3-max-2026-01-23',
    description:
      'qwen3-max model with thinking enabled from Coding Plan (Global/Intl)',
    baseUrl: CODING_PLAN_INTL_BASE_URL,
    envKey: CODING_PLAN_INTL_ENV_KEY,
    generationConfig: {
      extra_body: {
        enable_thinking: true,
      },
    },
  },
];

/**
 * Computes the version hash for the coding plan template.
 * Uses SHA256 of the JSON-serialized template for deterministic versioning.
 * @param template - The template to compute version for
 * @returns Hexadecimal string representing the template version
 */
export function computeCodingPlanVersion(template: CodingPlanTemplate): string {
  const templateString = JSON.stringify(template);
  return createHash('sha256').update(templateString).digest('hex');
}

/**
 * Current version of the China/Bailian coding plan template.
 * Computed at runtime from the template content.
 */
export const CODING_PLAN_VERSION = computeCodingPlanVersion(CODING_PLAN_MODELS);

/**
 * Current version of the Global/Intl coding plan template.
 * Computed at runtime from the template content.
 */
export const CODING_PLAN_INTL_VERSION = computeCodingPlanVersion(
  CODING_PLAN_INTL_MODELS,
);

/**
 * All coding plan templates for both regions.
 * Used for update detection and filtering.
 */
export const ALL_CODING_PLAN_TEMPLATES: CodingPlanTemplate = [
  ...CODING_PLAN_MODELS,
  ...CODING_PLAN_INTL_MODELS,
];

/**
 * Check if a config belongs to any Coding Plan template (China or Intl).
 * @param baseUrl - The baseUrl to check
 * @param envKey - The envKey to check
 * @param region - Optional region to limit the check to a specific region
 * @returns true if the config matches any Coding Plan template
 */
export function isCodingPlanConfig(
  baseUrl: string | undefined,
  envKey: string | undefined,
  region?: CodingPlanRegion,
): boolean {
  if (!baseUrl || !envKey) {
    return false;
  }

  // If region is specified, only check that region's templates
  if (region === CodingPlanRegion.GLOBAL) {
    return CODING_PLAN_INTL_MODELS.some(
      (template) => template.baseUrl === baseUrl && template.envKey === envKey,
    );
  } else if (region === CodingPlanRegion.CHINA) {
    return CODING_PLAN_MODELS.some(
      (template) => template.baseUrl === baseUrl && template.envKey === envKey,
    );
  }

  // No region specified, check all templates
  return ALL_CODING_PLAN_TEMPLATES.some(
    (template) => template.baseUrl === baseUrl && template.envKey === envKey,
  );
}

/**
 * Get the appropriate template and env key for the selected region.
 * @param region - The region to use (default: CHINA)
 * @returns Object containing template, envKey, version, and baseUrl
 */
export function getCodingPlanConfig(
  region: CodingPlanRegion = CodingPlanRegion.CHINA,
) {
  if (region === CodingPlanRegion.GLOBAL) {
    return {
      template: CODING_PLAN_INTL_MODELS,
      envKey: CODING_PLAN_INTL_ENV_KEY,
      version: CODING_PLAN_INTL_VERSION,
      baseUrl: CODING_PLAN_INTL_BASE_URL,
    };
  }
  return {
    template: CODING_PLAN_MODELS,
    envKey: CODING_PLAN_ENV_KEY,
    version: CODING_PLAN_VERSION,
    baseUrl: CODING_PLAN_BASE_URL,
  };
}
