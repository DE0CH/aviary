/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

import { useCallback, useState } from 'react';
import { Box, Text, useInput } from 'ink';
import { WizardStepProps } from './types.js';
import { UI } from './constants.js';
import { validateSubagentConfig } from './validation.js';
import { SubagentManager, SubagentConfig } from '@qwen-code/qwen-code-core';

/**
 * Step 6: Final confirmation and actions.
 */
export function CreationSummary({
  state,
  onPrevious: _onPrevious,
  onCancel,
}: WizardStepProps) {
  const [isSaving, setIsSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [saveSuccess, setSaveSuccess] = useState(false);

  const truncateText = (text: string, maxLength: number): string => {
    if (text.length <= maxLength) return text;
    return text.substring(0, maxLength - 3) + '...';
  };

  const toolsDisplay = Array.isArray(state.selectedTools)
    ? state.selectedTools.join(', ')
    : 'All available tools';

  const handleSave = useCallback(async () => {
    if (isSaving) return;

    setIsSaving(true);
    setSaveError(null);

    try {
      // Validate configuration before saving
      const configToValidate = {
        name: state.generatedName,
        description: state.generatedDescription,
        systemPrompt: state.generatedSystemPrompt,
        tools: state.selectedTools,
      };

      const validation = validateSubagentConfig(configToValidate);
      if (!validation.isValid) {
        throw new Error(`Validation failed: ${validation.errors.join(', ')}`);
      }

      // Create SubagentManager instance
      // TODO: Get project root from config or context
      const projectRoot = process.cwd();
      const subagentManager = new SubagentManager(projectRoot);

      // Build subagent configuration
      const config: SubagentConfig = {
        name: state.generatedName,
        description: state.generatedDescription,
        systemPrompt: state.generatedSystemPrompt,
        level: state.location,
        filePath: '', // Will be set by manager
        tools: Array.isArray(state.selectedTools)
          ? state.selectedTools
          : undefined,
        // TODO: Add modelConfig and runConfig if needed
      };

      // Create the subagent
      await subagentManager.createSubagent(config, {
        level: state.location,
        overwrite: false,
      });

      setSaveSuccess(true);

      // Auto-close after successful save
      setTimeout(() => {
        onCancel();
      }, UI.AUTO_CLOSE_DELAY_MS);
    } catch (error) {
      setSaveError(
        error instanceof Error ? error.message : 'Unknown error occurred',
      );
    } finally {
      setIsSaving(false);
    }
  }, [state, isSaving, onCancel]);

  const handleEdit = useCallback(() => {
    // TODO: Implement system editor integration
    setSaveError('Edit functionality not yet implemented');
  }, []);

  // Handle keyboard input
  useInput((input, key) => {
    if (isSaving || saveSuccess) return;

    if (key.return || input === 's') {
      handleSave();
      return;
    }

    if (input === 'e') {
      handleEdit();
      return;
    }
  });

  if (saveSuccess) {
    return (
      <Box flexDirection="column" gap={1}>
        <Box>
          <Text bold color="green">
            ✅ Subagent Created Successfully!
          </Text>
        </Box>
        <Box>
          <Text>
            Subagent &quot;{state.generatedName}&quot; has been saved to{' '}
            {state.location} level.
          </Text>
        </Box>
        <Box>
          <Text color="gray">Closing wizard...</Text>
        </Box>
      </Box>
    );
  }

  if (isSaving) {
    return (
      <Box flexDirection="column" gap={1}>
        <Box>
          <Text bold color="cyan">
            💾 Saving Subagent...
          </Text>
        </Box>
        <Box>
          <Text>Creating subagent &quot;{state.generatedName}&quot;...</Text>
        </Box>
      </Box>
    );
  }

  return (
    <Box flexDirection="column" gap={1}>
      {saveError && (
        <Box flexDirection="column" marginTop={1}>
          <Text bold color="red">
            ❌ Error saving subagent:
          </Text>
          <Text color="red" wrap="wrap">
            {saveError}
          </Text>
        </Box>
      )}

      <Box
        flexDirection="column"
      >
        <Box >
          <Text bold>Name: </Text>
          <Text>{state.generatedName}</Text>
        </Box>

        <Box>
          <Text bold>Location: </Text>
          <Text>
            {state.location === 'project'
              ? 'Project Level (.qwen/agents/)'
              : 'User Level (~/.qwen/agents/)'}
          </Text>
        </Box>

        <Box>
          <Text bold>Tools: </Text>
          <Text>{toolsDisplay}</Text>
        </Box>

        <Box marginTop={1}>
          <Text bold>Description:</Text>
        </Box>
        <Box>
          <Text wrap="wrap">
            {truncateText(state.generatedDescription, 200)}
          </Text>
        </Box>

        <Box marginTop={1}>
          <Text bold>System Prompt:</Text>
        </Box>
        <Box>
          <Text wrap="wrap">
            {truncateText(state.generatedSystemPrompt, 200)}
          </Text>
        </Box>

        <Box marginTop={1}>
          <Text bold>Background Color: </Text>
          <Text>
            {state.backgroundColor === 'auto'
              ? 'Automatic'
              : state.backgroundColor}
          </Text>
        </Box>
      </Box>
    </Box>
  );
}
