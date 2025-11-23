/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import type React from 'react';

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
      <svg
        xmlns="http://www.w3.org/2000/svg"
        viewBox="0 0 20 20"
        fill="currentColor"
        className="flex-shrink-0 w-5 h-5 mt-0.5"
        style={{ color: 'var(--app-warning-foreground)' }}
      >
        <path
          fillRule="evenodd"
          d="M8.485 2.495c.673-1.167 2.357-1.167 3.03 0l6.28 10.875c.673 1.167-.17 2.625-1.516 2.625H3.72c-1.347 0-2.189-1.458-1.515-2.625L8.485 2.495ZM10 5a.75.75 0 0 1 .75.75v3.5a.75.75 0 0 1-1.5 0v-3.5A.75.75 0 0 1 10 5Zm0 9a1 1 0 1 0 0-2 1 1 0 0 0 0 2Z"
          clipRule="evenodd"
        />
      </svg>

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
          <svg
            className="w-[10px] h-[10px]"
            width="10"
            height="10"
            viewBox="0 0 14 14"
            fill="none"
            xmlns="http://www.w3.org/2000/svg"
          >
            <path
              d="M1 1L13 13M1 13L13 1"
              stroke="currentColor"
              strokeWidth="1.5"
              strokeLinecap="round"
            />
          </svg>
        </button>
      )}
    </div>
  );
