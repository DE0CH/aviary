/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import type React from 'react';
import './EmptyState.css';
import { generateIconUrl } from '../utils/resourceUrl.js';

export const EmptyState: React.FC = () => {
  // Generate icon URL using the utility function
  const iconUri = generateIconUrl('icon.png');

  return (
    <div className="empty-state">
      <div className="empty-state-content">
        {/* Qwen Logo */}
        <div className="empty-state-logo">
          <img
            src={iconUri}
            alt="Qwen Logo"
            className="empty-state-logo-image"
          />
          <div className="empty-state-text">
            <div className="empty-state-title">
              What to do first? Ask about this codebase or we can start writing
              code.
            </div>
          </div>
        </div>
      </div>
    </div>
  );
};
