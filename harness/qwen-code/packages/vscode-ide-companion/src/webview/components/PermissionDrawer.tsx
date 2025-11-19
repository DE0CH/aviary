/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import type React from 'react';
import { useEffect } from 'react';
import {
  PermissionRequest,
  type PermissionOption,
  type ToolCall,
} from './PermissionRequest.js';
import './PermissionDrawer.css';

interface PermissionDrawerProps {
  isOpen: boolean;
  options: PermissionOption[];
  toolCall: ToolCall;
  onResponse: (optionId: string) => void;
  onClose?: () => void;
}

/**
 * Permission drawer component - displays permission requests in a bottom sheet
 * @param isOpen - Whether the drawer is open
 * @param options - Permission options to display
 * @param toolCall - Tool call information
 * @param onResponse - Callback when user responds
 * @param onClose - Optional callback when drawer closes
 */
export const PermissionDrawer: React.FC<PermissionDrawerProps> = ({
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
      <div className="permission-drawer-backdrop" onClick={onClose} />

      {/* Drawer */}
      <div className="permission-drawer">
        <div className="permission-drawer-header">
          <h3 className="permission-drawer-title">Permission Required</h3>
          {onClose && (
            <button
              className="permission-drawer-close"
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

        <div className="permission-drawer-content">
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
