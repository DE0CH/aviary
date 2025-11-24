/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import type React from 'react';
import { WarningTriangleIcon, CloseIcon } from './icons/index.js';

interface NotLoggedInMessageProps {
  /**
   * The message to display
   */
  message: string;

  /**
   * Callback when the login button is clicked
   */
  onLoginClick: () => void;

  /**
   * Callback when the message is dismissed (optional)
   */
  onDismiss?: () => void;
}

export const NotLoggedInMessage: React.FC<NotLoggedInMessageProps> = ({
  message,
  onLoginClick,
  onDismiss,
}) => (
  <div
    className="flex items-start gap-3 p-4 my-4 rounded-lg"
    style={{
      backgroundColor: 'var(--app-warning-background)',
      borderLeft: '3px solid var(--app-warning-border)',
    }}
  >
    {/* Warning Icon */}
    <WarningTriangleIcon
      className="flex-shrink-0 w-5 h-5 mt-0.5"
      style={{ color: 'var(--app-warning-foreground)' }}
    />

    {/* Content */}
    <div className="flex-1 min-w-0">
      <p
        className="m-0 mb-3 text-sm leading-relaxed"
        style={{ color: 'var(--app-primary-foreground)' }}
      >
        {message}
      </p>

      {/* Login Button */}
      <button
        className="px-4 py-2 text-sm font-medium rounded transition-colors duration-200"
        style={{
          backgroundColor: 'var(--app-qwen-orange)',
          color: 'white',
          border: 'none',
        }}
        onMouseEnter={(e) => {
          e.currentTarget.style.opacity = '0.9';
        }}
        onMouseLeave={(e) => {
          e.currentTarget.style.opacity = '1';
        }}
        onClick={() => {
          if (onDismiss) {
            onDismiss();
          }
          onLoginClick();
        }}
      >
        Login Now
      </button>
    </div>

    {/* Optional Close Button */}
    {onDismiss && (
      <button
        className="flex-shrink-0 flex items-center justify-center cursor-pointer rounded"
        style={{
          background: 'none',
          border: 'none',
          padding: '6px',
          color: 'var(--app-secondary-foreground)',
          borderRadius: '4px',
        }}
        onMouseEnter={(e) => {
          e.currentTarget.style.backgroundColor =
            'var(--app-ghost-button-hover-background)';
        }}
        onMouseLeave={(e) => {
          e.currentTarget.style.backgroundColor = 'transparent';
        }}
        aria-label="Dismiss"
        onClick={onDismiss}
      >
        <CloseIcon className="w-[10px] h-[10px]" />
      </button>
    )}
  </div>
);
