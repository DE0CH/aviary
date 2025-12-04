/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import type React from 'react';

interface InterruptedMessageProps {
  text?: string;
}

// A lightweight status line similar to WaitingMessage but without the left status icon.
export const InterruptedMessage: React.FC<InterruptedMessageProps> = ({
  text = 'Interrupted',
}) => (
  <div className="flex gap-0 items-start text-left py-2 flex-col opacity-85">
    <div
      className="qwen-message message-item interrupted-item"
      style={{
        width: '100%',
        alignItems: 'flex-start',
        paddingLeft: '30px', // keep alignment with other assistant messages, but no status icon
        position: 'relative',
        paddingTop: '8px',
        paddingBottom: '8px',
      }}
    >
      <span className="opacity-70 italic">{text}</span>
    </div>
  </div>
);
