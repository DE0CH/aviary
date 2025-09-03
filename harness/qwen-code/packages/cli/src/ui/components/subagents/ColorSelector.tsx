/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

import { Box, Text } from 'ink';
import { RadioButtonSelect } from '../shared/RadioButtonSelect.js';
import { WizardStepProps, ColorOption } from './types.js';
import { Colors } from '../../colors.js';

const colorOptions: ColorOption[] = [
  {
    id: 'auto',
    name: 'Automatic Color',
    value: 'auto',
  },
  {
    id: 'blue',
    name: 'Blue',
    value: '#3b82f6',
  },
  {
    id: 'green',
    name: 'Green',
    value: '#10b981',
  },
  {
    id: 'purple',
    name: 'Purple',
    value: '#8b5cf6',
  },
  {
    id: 'orange',
    name: 'Orange',
    value: '#f59e0b',
  },
  {
    id: 'red',
    name: 'Red',
    value: '#ef4444',
  },
  {
    id: 'cyan',
    name: 'Cyan',
    value: '#06b6d4',
  },
];

/**
 * Step 5: Background color selection with preview.
 */
export function ColorSelector({
  state,
  dispatch,
  onNext,
  onPrevious: _onPrevious,
}: WizardStepProps) {
  const handleSelect = (_selectedValue: string) => {
    onNext();
  };

  const handleHighlight = (selectedValue: string) => {
    const colorOption = colorOptions.find(
      (option) => option.id === selectedValue,
    );
    if (colorOption) {
      dispatch({ type: 'SET_BACKGROUND_COLOR', color: colorOption.value });
    }
  };

  const currentColor =
    colorOptions.find((option) => option.value === state.backgroundColor) ||
    colorOptions[0];

  return (
    <Box flexDirection="column" gap={1}>
      <Box flexDirection="column">
        <RadioButtonSelect
          items={colorOptions.map((option) => ({
            label: option.name,
            value: option.id,
          }))}
          initialIndex={colorOptions.findIndex(
            (opt) => opt.id === currentColor.id,
          )}
          onSelect={handleSelect}
          onHighlight={handleHighlight}
          isFocused={true}
        />
      </Box>

      <Box flexDirection="row">
        <Text color={Colors.Gray}>Preview:</Text>
        <Box marginLeft={2}>
          <Text color={currentColor.value}>{state.generatedName}</Text>
        </Box>
      </Box>
    </Box>
  );
}
