/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 *
 * PermissionDrawer component using Tailwind CSS
 */

import type React from 'react';
import { useEffect } from 'react';
import {
  PermissionRequest,
  type PermissionOption,
  type ToolCall,
} from './PermissionRequest.js';
import { buttonClasses, commonClasses } from '../../lib/tailwindUtils.js';

interface PermissionDrawerProps {
  isOpen: boolean;
  options: PermissionOption[];
  toolCall: ToolCall;
  onResponse: (optionId: string) => void;
  onClose?: () => void;
}

/**
 * Permission drawer component - displays permission requests in a bottom sheet
 * Uses Tailwind CSS for styling
 * @param isOpen - Whether the drawer is open
 * @param options - Permission options to display
 * @param toolCall - Tool call information
 * @param onResponse - Callback when user responds
 * @param onClose - Optional callback when drawer closes
 */
export const PermissionDrawerTailwind: React.FC<PermissionDrawerProps> = ({
  isOpen,
  options,
  toolCall,
  onResponse,
  onClose,
}) => {
  // Close drawer on Escape key
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (!isOpen) {
        return;
      }

      // Close on Escape
      if (e.key === 'Escape' && onClose) {
        onClose();
        return;
      }

      // Quick select with number keys (1-9)
      const numMatch = e.key.match(/^[1-9]$/);
      if (numMatch) {
        const index = parseInt(e.key, 10) - 1;
        if (index < options.length) {
          e.preventDefault();
          onResponse(options[index].optionId);
        }
      }
    };

    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [isOpen, onClose, options, onResponse]);

  if (!isOpen) {
    return null;
  }

  return (
    <>
      {/* Backdrop */}
      <div
        className="fixed inset-0 bg-black bg-opacity-75 z-[998] animate-fadeIn"
        onClick={onClose}
      />

      {/* Drawer */}
      <div className="flex flex-col p-2 bg-gray-100 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-large max-h-[70vh] outline-0 relative mb-1.5 z-[999] animate-slideUpFromBottom">
        <div className="bg-white dark:bg-gray-900 rounded-large absolute inset-0"></div>
        <div className="relative flex items-center justify-between p-7 border-b border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-800 rounded-t-2xl shadow-sm flex-shrink-0">
          <h3 className="font-bold text-gray-900 dark:text-white mb-1">Permission Required</h3>
          {onClose && (
            <button
              className={buttonClasses('icon')}
              onClick={onClose}
              aria-label="Close drawer"
            >
              <svg
                width="16"
                height="16"
                viewBox="0 0 16 16"
                fill="none"
                xmlns="http://www.w3.org/2000/svg"
              >
                <path
                  d="M2 2L14 14M2 14L14 2"
                  stroke="currentColor"
                  strokeWidth="1.5"
                  strokeLinecap="round"
                />
              </svg>
            </button>
          )}
        </div>

        <div className="text-lg text-gray-900 dark:text-white flex flex-col min-h-0 z-10 flex-1 overflow-y-auto p-0 min-h-0">
          <PermissionRequest
            options={options}
            toolCall={toolCall}
            onResponse={onResponse}
          />
        </div>
      </div>
    </>
  );
};