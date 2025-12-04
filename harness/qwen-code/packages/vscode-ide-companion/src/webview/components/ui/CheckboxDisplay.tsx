/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import type React from 'react';

export interface CheckboxDisplayProps {
  checked?: boolean;
  indeterminate?: boolean;
  disabled?: boolean;
  className?: string;
  style?: React.CSSProperties;
  title?: string;
}

/**
 * Display-only checkbox styled via Tailwind classes.
 * - Renders a custom-looking checkbox using appearance-none and pseudo-elements.
 * - Supports indeterminate (middle) state using the DOM property and a data- attribute.
 * - Intended for read-only display (disabled by default).
 */
export const CheckboxDisplay: React.FC<CheckboxDisplayProps> = ({
  checked = false,
  indeterminate = false,
  disabled = true,
  className = '',
  style,
  title,
}) => {
  // Render as a span (not <input>) so we can draw a checkmark with CSS.
  // Pseudo-elements do not reliably render on <input> in Chromium (VS Code webviews),
  // which caused the missing icon. This version is font-free and uses borders.
  const showCheck = !!checked && !indeterminate;
  const showDash = !!indeterminate;

  return (
    <span
      role="checkbox"
      aria-checked={indeterminate ? 'mixed' : !!checked}
      aria-disabled={disabled || undefined}
      title={title}
      style={style}
      className={[
        'q m-[2px] shrink-0 w-4 h-4 relative rounded-[2px] box-border',
        'border border-[var(--app-input-border)] bg-[var(--app-input-background)]',
        'inline-flex items-center justify-center',
        showCheck ? 'opacity-70' : '',
        className,
      ].join(' ')}
    >
      {showCheck ? (
        <span
          aria-hidden
          className={[
            'absolute block',
            // Place the check slightly to the left/top so rotated arms stay inside the 16x16 box
            'left-[3px] top-[3px]',
            // 10x6 shape works well for a 16x16 checkbox
            'w-2.5 h-1.5',
            // Draw the L-corner and rotate to form a check
            'border-l-2 border-b-2',
            'border-[#74c991]',
            '-rotate-45',
          ].join(' ')}
        />
      ) : null}
      {showDash ? (
        <span
          aria-hidden
          className={[
            'absolute block',
            'left-1/2 top-1/2 -translate-x-1/2 -translate-y-1/2',
            'w-2 h-[2px] rounded-sm',
            'bg-[#e1c08d]',
          ].join(' ')}
        />
      ) : null}
    </span>
  );
};
