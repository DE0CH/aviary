// Type declarations for tailwindUtils.js

export function buttonClasses(
  variant?: 'primary' | 'secondary' | 'ghost' | 'icon',
  disabled?: boolean,
): string;

export function inputClasses(): string;

export function cardClasses(): string;

export function dialogClasses(): string;

export function qwenColorClasses(
  color: 'orange' | 'clay-orange' | 'ivory' | 'slate' | 'green',
): string;

export function spacingClasses(
  size?: 'small' | 'medium' | 'large' | 'xlarge',
  direction?: 'all' | 'x' | 'y' | 't' | 'r' | 'b' | 'l',
): string;

export function borderRadiusClasses(
  size?: 'small' | 'medium' | 'large',
): string;

export const commonClasses: {
  flexCenter: string;
  flexBetween: string;
  flexCol: string;
  textMuted: string;
  textSmall: string;
  textLarge: string;
  fontWeightMedium: string;
  fontWeightSemibold: string;
  marginAuto: string;
  fullWidth: string;
  fullHeight: string;
  truncate: string;
  srOnly: string;
  transition: string;
};
