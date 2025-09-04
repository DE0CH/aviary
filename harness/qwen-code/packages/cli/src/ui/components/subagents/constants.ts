/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * Constants for the subagent creation wizard.
 */

// Wizard step numbers
export const WIZARD_STEPS = {
  LOCATION_SELECTION: 1,
  GENERATION_METHOD: 2,
  DESCRIPTION_INPUT: 3,
  TOOL_SELECTION: 4,
  COLOR_SELECTION: 5,
  FINAL_CONFIRMATION: 6,
} as const;

// Total number of wizard steps
export const TOTAL_WIZARD_STEPS = 6;

// Step names for display
export const STEP_NAMES: Record<number, string> = {
  [WIZARD_STEPS.LOCATION_SELECTION]: 'Location Selection',
  [WIZARD_STEPS.GENERATION_METHOD]: 'Generation Method',
  [WIZARD_STEPS.DESCRIPTION_INPUT]: 'Description Input',
  [WIZARD_STEPS.TOOL_SELECTION]: 'Tool Selection',
  [WIZARD_STEPS.COLOR_SELECTION]: 'Color Selection',
  [WIZARD_STEPS.FINAL_CONFIRMATION]: 'Final Confirmation',
};
