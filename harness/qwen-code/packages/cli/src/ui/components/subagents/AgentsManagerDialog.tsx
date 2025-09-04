/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

import { useReducer, useCallback, useMemo } from 'react';
import { Box, Text, useInput } from 'ink';
import { managementReducer, initialManagementState } from './reducers.js';
import { AgentSelectionStep } from './AgentSelectionStep.js';
import { ActionSelectionStep } from './ActionSelectionStep.js';
import { AgentViewerStep } from './AgentViewerStep.js';
import { ManagementStepProps, MANAGEMENT_STEPS } from './types.js';
import { Colors } from '../../colors.js';
import { theme } from '../../semantic-colors.js';
import { Config } from '@qwen-code/qwen-code-core';

interface AgentsManagerDialogProps {
  onClose: () => void;
  config: Config | null;
}

/**
 * Main orchestrator component for the agents management dialog.
 */
export function AgentsManagerDialog({
  onClose,
  config,
}: AgentsManagerDialogProps) {
  const [state, dispatch] = useReducer(
    managementReducer,
    initialManagementState,
  );

  const handleNext = useCallback(() => {
    dispatch({ type: 'GO_TO_NEXT_STEP' });
  }, []);

  const handlePrevious = useCallback(() => {
    dispatch({ type: 'GO_TO_PREVIOUS_STEP' });
  }, []);

  const handleCancel = useCallback(() => {
    dispatch({ type: 'RESET_DIALOG' });
    onClose();
  }, [onClose]);

  // Centralized ESC key handling for the entire dialog
  useInput((input, key) => {
    if (key.escape) {
      // Agent viewer step handles its own ESC logic
      if (state.currentStep === MANAGEMENT_STEPS.AGENT_VIEWER) {
        return; // Let AgentViewerStep handle it
      }

      if (state.currentStep === MANAGEMENT_STEPS.AGENT_SELECTION) {
        // On first step, ESC cancels the entire dialog
        handleCancel();
      } else {
        // On other steps, ESC goes back to previous step
        handlePrevious();
      }
    }
  });

  const stepProps: ManagementStepProps = useMemo(
    () => ({
      state,
      config,
      dispatch,
      onNext: handleNext,
      onPrevious: handlePrevious,
      onCancel: handleCancel,
    }),
    [state, dispatch, handleNext, handlePrevious, handleCancel, config],
  );

  const renderStepHeader = useCallback(() => {
    const getStepHeaderText = () => {
      switch (state.currentStep) {
        case MANAGEMENT_STEPS.AGENT_SELECTION:
          return 'Agents';
        case MANAGEMENT_STEPS.ACTION_SELECTION:
          return 'Choose Action';
        case MANAGEMENT_STEPS.AGENT_VIEWER:
          return state.selectedAgent?.name;
        case MANAGEMENT_STEPS.AGENT_EDITOR:
          return `Editing: ${state.selectedAgent?.name || 'Unknown'}`;
        case MANAGEMENT_STEPS.DELETE_CONFIRMATION:
          return `Delete: ${state.selectedAgent?.name || 'Unknown'}`;
        default:
          return 'Unknown Step';
      }
    };

    return (
      <Box>
        <Text bold>{getStepHeaderText()}</Text>
      </Box>
    );
  }, [state.currentStep, state.selectedAgent?.name]);

  const renderStepFooter = useCallback(() => {
    const getNavigationInstructions = () => {
      if (state.currentStep === MANAGEMENT_STEPS.ACTION_SELECTION) {
        return 'Enter to select, ↑↓ to navigate, Esc to go back';
      }

      if (state.currentStep === MANAGEMENT_STEPS.AGENT_SELECTION) {
        if (state.availableAgents.length === 0) {
          return 'Esc to close';
        }
        return 'Enter to select, ↑↓ to navigate, Esc to close';
      }

      return 'Esc to go back';
    };

    return (
      <Box>
        <Text color={theme.text.secondary}>{getNavigationInstructions()}</Text>
      </Box>
    );
  }, [state.currentStep, state.availableAgents.length]);

  const renderStepContent = useCallback(() => {
    switch (state.currentStep) {
      case MANAGEMENT_STEPS.AGENT_SELECTION:
        return <AgentSelectionStep {...stepProps} />;
      case MANAGEMENT_STEPS.ACTION_SELECTION:
        return <ActionSelectionStep {...stepProps} />;
      case MANAGEMENT_STEPS.AGENT_VIEWER:
        return <AgentViewerStep {...stepProps} />;
      case MANAGEMENT_STEPS.AGENT_EDITOR:
        return (
          <Box>
            <Text color={theme.status.warning}>
              Agent editing not yet implemented
            </Text>
          </Box>
        );
      case MANAGEMENT_STEPS.DELETE_CONFIRMATION:
        return (
          <Box>
            <Text color={theme.status.warning}>
              Agent deletion not yet implemented
            </Text>
          </Box>
        );
      default:
        return (
          <Box>
            <Text color={theme.status.error}>
              Invalid step: {state.currentStep}
            </Text>
          </Box>
        );
    }
  }, [stepProps, state.currentStep]);

  return (
    <Box flexDirection="column">
      {/* Main content wrapped in bounding box */}
      <Box
        borderStyle="single"
        borderColor={Colors.Gray}
        flexDirection="column"
        padding={1}
        width="100%"
        gap={1}
      >
        {renderStepHeader()}
        {renderStepContent()}
        {renderStepFooter()}
      </Box>
    </Box>
  );
}
