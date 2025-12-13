/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import type React from 'react';
import { generateIconUrl } from '../../utils/resourceUrl.js';

interface EmptyStateProps {
  isAuthenticated?: boolean;
  loadingMessage?: string;
}

export const EmptyState: React.FC<EmptyStateProps> = ({
  isAuthenticated = false,
  loadingMessage,
}) => {
  // Generate icon URL using the utility function
  const iconUri = generateIconUrl('icon.png');

  const description = loadingMessage
    ? 'Preparing Qwen Code…'
    : isAuthenticated
      ? 'What would you like to do? Ask about this codebase or we can start writing code.'
      : 'Welcome! Please log in to start using Qwen Code.';

  return (
    <div className="flex flex-col items-center justify-center h-full p-5 md:p-10">
      {/* Loading overlay */}
      {loadingMessage && (
        <div className="bg-background/80 absolute inset-0 z-50 flex items-center justify-center backdrop-blur-sm">
          <div className="text-center">
            <div className="border-primary mx-auto mb-2 h-8 w-8 animate-spin rounded-full border-b-2"></div>
            <p className="text-muted-foreground text-sm">{loadingMessage}</p>
          </div>
        </div>
      )}

      <div className="flex flex-col items-center gap-8 w-full">
        {/* Qwen Logo */}
        <div className="flex flex-col items-center gap-6">
          <img
            src={iconUri}
            alt="Qwen Logo"
            className="w-[60px] h-[60px] object-contain"
          />
          <div className="text-center">
            <div className="text-[15px] text-app-primary-foreground leading-normal font-normal max-w-[400px]">
              {description}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
};
