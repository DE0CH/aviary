/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * Validation result interface.
 */
export interface ValidationResult {
  isValid: boolean;
  errors: string[];
}

/**
 * Sanitizes user input by removing dangerous characters and normalizing whitespace.
 */
export function sanitizeInput(input: string): string {
  return (
    input
      .trim()
      // eslint-disable-next-line no-control-regex
      .replace(/[\x00-\x08\x0B\x0C\x0E-\x1F\x7F]/g, '') // Remove control characters
      .replace(/\s+/g, ' ') // Normalize whitespace
  ); // Limit length
}

/**
 * Validates a system prompt.
 */
export function validateSystemPrompt(prompt: string): ValidationResult {
  const errors: string[] = [];
  const sanitized = sanitizeInput(prompt);

  if (sanitized.length === 0) {
    errors.push('System prompt cannot be empty');
  }

  if (sanitized.length > 5000) {
    errors.push('System prompt is too long (maximum 5000 characters)');
  }

  return {
    isValid: errors.length === 0,
    errors,
  };
}

/**
 * Validates tool selection.
 */
export function validateToolSelection(
  tools: string[] | 'all',
): ValidationResult {
  const errors: string[] = [];

  if (Array.isArray(tools)) {
    if (tools.length === 0) {
      errors.push('At least one tool must be selected');
    }

    // Check for valid tool names (basic validation)
    const invalidTools = tools.filter(
      (tool) =>
        typeof tool !== 'string' ||
        tool.trim().length === 0 ||
        !/^[a-zA-Z0-9_-]+$/.test(tool),
    );

    if (invalidTools.length > 0) {
      errors.push(`Invalid tool names: ${invalidTools.join(', ')}`);
    }
  }

  return {
    isValid: errors.length === 0,
    errors,
  };
}

/**
 * Comprehensive validation for the entire subagent configuration.
 */
export function validateSubagentConfig(config: {
  name: string;
  description: string;
  systemPrompt: string;
  tools: string[] | 'all';
}): ValidationResult {
  const errors: string[] = [];

  const promptValidation = validateSystemPrompt(config.systemPrompt);
  if (!promptValidation.isValid) {
    errors.push(...promptValidation.errors);
  }

  const toolsValidation = validateToolSelection(config.tools);
  if (!toolsValidation.isValid) {
    errors.push(...toolsValidation.errors);
  }

  return {
    isValid: errors.length === 0,
    errors,
  };
}
