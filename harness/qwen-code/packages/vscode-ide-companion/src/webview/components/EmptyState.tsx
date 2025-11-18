/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import type React from 'react';
import './EmptyState.css';

// Extend Window interface to include ICON_URI
declare global {
  interface Window {
    ICON_URI?: string;
  }
}

export const EmptyState: React.FC = () => {
  // Get icon URI from window, fallback to empty string if not available
  const iconUri = window.ICON_URI || '';

  return (
    <div className="empty-state">
      <div className="empty-state-content">
        {/* Qwen Logo */}
        <div className="empty-state-logo">
          {iconUri && (
            <img
              src={iconUri}
              alt="Qwen Logo"
              className="empty-state-logo-image"
            />
          )}
          <div className="empty-state-text">
            <div className="empty-state-title">
              What to do first? Ask about this codebase or we can start writing
              code.
            </div>
          </div>
        </div>

        {/* Info Banner */}
        <div className="empty-state-banner">
          <div className="banner-content">
            <svg
              className="banner-icon"
              width="16"
              height="16"
              viewBox="0 0 20 20"
              xmlns="http://www.w3.org/2000/svg"
            >
              <path d="M5.14648 7.14648C5.34175 6.95122 5.65825 6.95122 5.85352 7.14648L8.35352 9.64648C8.44728 9.74025 8.5 9.86739 8.5 10C8.5 10.0994 8.47037 10.1958 8.41602 10.2773L8.35352 10.3535L5.85352 12.8535C5.65825 13.0488 5.34175 13.0488 5.14648 12.8535C4.95122 12.6583 4.95122 12.3417 5.14648 12.1465L7.29297 10L5.14648 7.85352C4.95122 7.65825 4.95122 7.34175 5.14648 7.14648Z"></path>
              <path d="M14.5 12C14.7761 12 15 12.2239 15 12.5C15 12.7761 14.7761 13 14.5 13H9.5C9.22386 13 9 12.7761 9 12.5C9 12.2239 9.22386 12 9.5 12H14.5Z"></path>
              <path
                fillRule="evenodd"
                clipRule="evenodd"
                d="M16.5 4C17.3284 4 18 4.67157 18 5.5V14.5C18 15.3284 17.3284 16 16.5 16H3.5C2.67157 16 2 15.3284 2 14.5V5.5C2 4.67157 2.67157 4 3.5 4H16.5ZM3.5 5C3.22386 5 3 5.22386 3 5.5V14.5C3 14.7761 3.22386 15 3.5 15H16.5C16.7761 15 17 14.7761 17 14.5V5.5C17 5.22386 16.7761 5 16.5 5H3.5Z"
              ></path>
            </svg>
            <label>
              Prefer the Terminal experience?{' '}
              <a href="#" className="banner-link">
                Switch back in Settings.
              </a>
            </label>
          </div>
          <button className="banner-close" aria-label="Close banner">
            <svg
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
              ></path>
            </svg>
          </button>
        </div>
      </div>
    </div>
  );
};
