import { COLOR_OPTIONS } from './constants.js';

export const shouldShowColor = (backgroundColor?: string): boolean =>
  backgroundColor !== undefined && backgroundColor !== 'auto';

export const getColorForDisplay = (colorName?: string): string | undefined =>
  !colorName || colorName === 'auto'
    ? undefined
    : COLOR_OPTIONS.find((color) => color.name === colorName)?.value;
