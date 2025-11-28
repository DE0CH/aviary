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
    className="flex gap-1 select-none py-1.5 px-2.5"
    style={{
      backgroundColor: 'var(--app-header-background)',
    }}
  >
    {/* Past Conversations Button */}
    <button
      className="flex-none py-1 px-2 bg-transparent border border-transparent rounded cursor-pointer flex items-center justify-center outline-none font-medium transition-colors duration-200 hover:bg-[var(--app-ghost-button-hover-background)] focus:bg-[var(--app-ghost-button-hover-background)]"
      style={{
        borderRadius: 'var(--corner-radius-small)',
        color: 'var(--app-primary-foreground)',
        fontSize: 'var(--vscode-chat-font-size, 13px)',
      }}
      onClick={onLoadSessions}
      title="Past conversations"
    >
      <span className="flex items-center gap-1">
        <span style={{ fontSize: 'var(--vscode-chat-font-size, 13px)' }}>
          {currentSessionTitle}
        </span>
        <ChevronDownIcon className="w-3.5 h-3.5" />
      </span>
    </button>

    {/* Spacer */}
    <div className="flex-1"></div>

    {/* New Session Button */}
    <button
      className="flex-none p-0 bg-transparent border border-transparent rounded cursor-pointer flex items-center justify-center outline-none w-6 h-6 hover:bg-[var(--app-ghost-button-hover-background)] focus:bg-[var(--app-ghost-button-hover-background)]"
      style={{
        color: 'var(--app-primary-foreground)',
      }}
      onClick={onNewSession}
      title="New Session"
    >
      <PlusIcon className="w-4 h-4" />
    </button>
  </div>
);
