/**
 * @license
 * Copyright 2025 Qwen Team
 * SPDX-License-Identifier: Apache-2.0
 */

import type React from 'react';
import { ChevronDownIcon, PlusIcon } from '../icons/index.js';

interface ChatHeaderProps {
  currentSessionTitle: string;
  onLoadSessions: () => void;
  onSaveSession: () => void;
  onNewSession: () => void;
}

export const ChatHeader: React.FC<ChatHeaderProps> = ({
  currentSessionTitle,
  onLoadSessions,
  onSaveSession: _onSaveSession,
  onNewSession,
}) => (
  <div
    className="chat-header flex items-center select-none py-1.5 px-2.5 w-full"
    style={{
      backgroundColor: 'var(--app-header-background)',
    }}
  >
    <button
      className="btn-ghost btn-md px-2 flex items-center outline-none font-medium max-w-[70%] min-w-0 overflow-hidden rounded hover:bg-[var(--app-ghost-button-hover-background)] h-6 leading-6"
      onClick={onLoadSessions}
      title="Past conversations"
    >
      <span className="whitespace-nowrap overflow-hidden text-ellipsis min-w-0">
        {currentSessionTitle}
      </span>
      <ChevronDownIcon className="w-4 h-4 flex-shrink-0 ml-1" />
    </button>

    <div className="flex-1 min-w-2"></div>

    <button
      className="btn-ghost btn-sm flex items-center justify-center outline-none rounded hover:bg-[var(--app-ghost-button-hover-background)] h-6 leading-6 w-6"
      onClick={onNewSession}
      title="New Session"
    >
      <PlusIcon className="w-4 h-4" />
    </button>
  </div>
);
