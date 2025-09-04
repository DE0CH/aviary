/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

import { Box } from 'ink';
import { RadioButtonSelect } from '../shared/RadioButtonSelect.js';
import { ManagementStepProps } from './types.js';

export const ActionSelectionStep = ({
  state,
  dispatch,
  onNext,
  onPrevious,
}: ManagementStepProps) => {
  const actions = [
    { label: 'View Agent', value: 'view' as const },
    { label: 'Edit Agent', value: 'edit' as const },
    { label: 'Delete Agent', value: 'delete' as const },
    { label: 'Back', value: 'back' as const },
  ];

  const handleActionSelect = (value: 'view' | 'edit' | 'delete' | 'back') => {
    if (value === 'back') {
      onPrevious();
      return;
    }

    dispatch({ type: 'SELECT_ACTION', payload: value });
    onNext();
  };

  const selectedIndex = state.selectedAction
    ? actions.findIndex((action) => action.value === state.selectedAction)
    : 0;

  return (
    <Box flexDirection="column">
      <RadioButtonSelect
        items={actions}
        initialIndex={selectedIndex}
        onSelect={handleActionSelect}
        showNumbers={false}
      />
    </Box>
  );
};
