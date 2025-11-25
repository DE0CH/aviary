/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import type React from 'react';

interface WaitingMessageProps {
  loadingMessage: string;
}

export const WaitingMessage: React.FC<WaitingMessageProps> = ({
  loadingMessage,
}) => (
  <div className="flex gap-0 items-start text-left py-2 flex-col opacity-85 animate-[fadeIn_0.2s_ease-in]">
    <div className="bg-transparent border-0 py-2 flex items-center gap-2">
      <span className="inline-flex items-center gap-1 mr-0">
        <span className="inline-block w-1.5 h-1.5 bg-[var(--app-secondary-foreground)] rounded-full mr-0 opacity-60 animate-[typingPulse_1.4s_infinite_ease-in-out] [animation-delay:0s]"></span>
        <span className="inline-block w-1.5 h-1.5 bg-[var(--app-secondary-foreground)] rounded-full mr-0 opacity-60 animate-[typingPulse_1.4s_infinite_ease-in-out] [animation-delay:0.2s]"></span>
        <span className="inline-block w-1.5 h-1.5 bg-[var(--app-secondary-foreground)] rounded-full mr-0 opacity-60 animate-[typingPulse_1.4s_infinite_ease-in-out] [animation-delay:0.4s]"></span>
      </span>
      <span
        className="opacity-70 italic"
        style={{ color: 'var(--app-secondary-foreground)' }}
      >
        {loadingMessage}
      </span>
    </div>
  </div>
);
