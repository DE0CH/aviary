/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

import { Box, Text } from 'ink';
import { RadioButtonSelect } from '../shared/RadioButtonSelect.js';
import { WizardStepProps, ColorOption } from './types.js';
import { Colors } from '../../colors.js';
import { COLOR_OPTIONS } from './constants.js';

const colorOptions: ColorOption[] = COLOR_OPTIONS;

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
      dispatch({ type: 'SET_BACKGROUND_COLOR', color: colorOption.name });
    }
  };

  const currentColor =
    colorOptions.find((option) => option.name === state.backgroundColor) ||
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
        <Box marginLeft={2} backgroundColor={currentColor.value}>
          <Text color="black">{` ${state.generatedName} `}</Text>
        </Box>
      </Box>
    </Box>
  );
}
