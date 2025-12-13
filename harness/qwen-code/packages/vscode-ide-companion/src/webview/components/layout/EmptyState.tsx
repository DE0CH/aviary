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
            {loadingMessage && (
              <div className="flex items-center justify-center gap-2 mt-4 text-sm text-app-secondary-foreground">
                <span className="inline-block h-3 w-3 rounded-full border-2 border-app-secondary-foreground/40 border-t-app-primary-foreground animate-spin" />
                <span>{loadingMessage}</span>
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
};
